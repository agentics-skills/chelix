use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use {
    async_trait::async_trait,
    moltis_agents::tool_registry::AgentTool,
    serde::{Deserialize, Serialize},
    tokio::sync::{RwLock, Semaphore},
    tracing::{debug, info},
};

use crate::{
    Result,
    error::Error,
    exec::{ExecOpts, ExecResult},
    params::without_null_params,
    sandbox::SandboxRouter,
};

const DEFAULT_TIMEOUT_MILLIS: u64 = 300_000;
const MAX_TIMEOUT_MILLIS: u64 = 1_800_000;
const DEFAULT_CAPTURE_LINES: usize = 1_000;
const MAX_CAPTURE_LINES: usize = 20_000;
const DEFAULT_TMUX_COLS: u16 = 200;
const DEFAULT_TMUX_ROWS: u16 = 50;
const SANDBOX_WORKDIR: &str = "/home/sandbox";
const FIELD_SEP: &str = "|moltis-tmux-field|";
const START_PREFIX: &str = "__MOLTIS_EXEC_START__";
const DONE_PREFIX: &str = "__MOLTIS_EXEC_DONE__";

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExecuteCommandParams {
    command: String,
    #[serde(default)]
    custom_cwd: Option<String>,
    #[serde(default)]
    new_terminal: bool,
    #[serde(default)]
    destructive_flag: Option<bool>,
    #[serde(default)]
    background: bool,
    #[serde(default)]
    timeout: Option<u64>,
    #[serde(default)]
    terminal_id: Option<String>,
    #[serde(rename = "_session_key")]
    #[serde(default)]
    _session_key: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReadTerminalOutputParams {
    terminal_id: String,
    #[serde(default)]
    max_lines: Option<usize>,
    #[serde(rename = "_session_key")]
    #[serde(default)]
    _session_key: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExecuteCommandResponse {
    terminal_id: String,
    run_id: String,
    session_id: String,
    session_name: String,
    window_id: String,
    window_name: String,
    pane_id: String,
    output: String,
    exit_code: Option<i32>,
    completed: bool,
    timed_out: bool,
    background: bool,
    message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReadTerminalOutputResponse {
    terminal_id: String,
    session_id: String,
    session_name: String,
    window_id: String,
    window_name: String,
    pane_id: String,
    output: String,
    exit_code: Option<i32>,
    completed: bool,
    running: bool,
}

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

#[derive(Debug, Clone)]
struct CaptureResult {
    output: String,
    completed: bool,
    exit_code: Option<i32>,
}

pub struct TmuxTerminalManager {
    sandbox_router: Arc<SandboxRouter>,
    terminals: RwLock<HashMap<String, ManagedTerminal>>,
    next_id: AtomicU64,
    max_output_bytes: usize,
}

impl TmuxTerminalManager {
    #[must_use]
    pub fn new(sandbox_router: Arc<SandboxRouter>, max_output_bytes: usize) -> Self {
        Self {
            sandbox_router,
            terminals: RwLock::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            max_output_bytes,
        }
    }

    async fn execute_command(
        &self,
        session_key: &str,
        params: ExecuteCommandParams,
    ) -> Result<ExecuteCommandResponse> {
        let command = params.command.trim();
        if command.is_empty() {
            return Err(Error::message("command cannot be empty"));
        }

        if params.destructive_flag.unwrap_or(false) {
            debug!("execute_command destructive_flag is accepted for compatibility");
        }

        let timeout_millis = params
            .timeout
            .unwrap_or(DEFAULT_TIMEOUT_MILLIS)
            .min(MAX_TIMEOUT_MILLIS);
        let timeout = Duration::from_millis(timeout_millis);
        let terminal = self
            .resolve_terminal(
                session_key,
                params.terminal_id.as_deref(),
                params.new_terminal,
                params.custom_cwd.as_deref(),
            )
            .await?;

        let permit = terminal
            .gate
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| Error::message("terminal gate closed"))?;
        let terminal = self.refresh_terminal_completion(terminal).await?;
        if terminal.is_running() {
            return Err(Error::message(format!(
                "terminal {} is still running a command; use read_terminal_output or newTerminal=true",
                terminal.id
            )));
        }

        let run_id = uuid::Uuid::new_v4().simple().to_string();
        let capture_before = self
            .capture_pane(&terminal, MAX_CAPTURE_LINES)
            .await
            .unwrap_or_default();
        let baseline_lines = capture_before.lines().count();
        let marker_enabled = true;
        let paste_text = build_paste_payload(
            command,
            params.custom_cwd.as_deref(),
            &run_id,
            marker_enabled,
        );

        self.paste_text(&terminal, &paste_text).await?;

        let mut updated = terminal.clone();
        updated.last_run = Some(ManagedRun {
            run_id: run_id.clone(),
            command: command.to_string(),
            baseline_lines,
            marker_enabled,
            completed: false,
            exit_code: None,
        });
        self.store_terminal(updated.clone()).await;

        if params.background {
            let terminal_id = updated.id.clone();
            drop(permit);
            return Ok(ExecuteCommandResponse {
                terminal_id,
                run_id,
                session_id: updated.session_id,
                session_name: updated.session_name,
                window_id: updated.window_id,
                window_name: updated.window_name,
                pane_id: updated.pane_id,
                output: String::new(),
                exit_code: None,
                completed: false,
                timed_out: false,
                background: true,
                message: "Command started in sandbox tmux terminal".to_string(),
            });
        }

        let deadline = tokio::time::Instant::now() + timeout;
        let last_capture = loop {
            let raw = self.capture_pane(&updated, MAX_CAPTURE_LINES).await?;
            let capture = extract_run_capture(&raw, updated.last_run.as_ref());
            if capture.completed || tokio::time::Instant::now() >= deadline {
                break capture;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        };

        if last_capture.completed {
            self.mark_run_completed(&updated.id, last_capture.exit_code)
                .await;
        }
        drop(permit);

        let timed_out = !last_capture.completed;
        let mut output = last_capture.output;
        truncate_output_for_display(&mut output, self.max_output_bytes);
        let terminal_id = updated.id.clone();
        let message = if timed_out {
            format!(
                "Command still running in sandbox tmux terminal (id: {terminal_id}) after {timeout_millis}ms"
            )
        } else {
            format!("Command finished in sandbox tmux terminal (id: {terminal_id})")
        };

        Ok(ExecuteCommandResponse {
            terminal_id,
            run_id,
            session_id: updated.session_id,
            session_name: updated.session_name,
            window_id: updated.window_id,
            window_name: updated.window_name,
            pane_id: updated.pane_id,
            output,
            exit_code: last_capture.exit_code,
            completed: last_capture.completed,
            timed_out,
            background: false,
            message,
        })
    }

    async fn read_terminal_output(
        &self,
        session_key: &str,
        params: ReadTerminalOutputParams,
    ) -> Result<ReadTerminalOutputResponse> {
        let terminal = self
            .lookup_terminal(session_key, &params.terminal_id)
            .await?;
        let terminal = self.refresh_terminal_completion(terminal).await?;
        let max_lines = params
            .max_lines
            .unwrap_or(DEFAULT_CAPTURE_LINES)
            .clamp(1, MAX_CAPTURE_LINES);
        let raw = self.capture_pane(&terminal, max_lines).await?;
        let capture = extract_run_capture(&raw, terminal.last_run.as_ref());
        if capture.completed {
            self.mark_run_completed(&terminal.id, capture.exit_code)
                .await;
        }
        let running = !capture.completed && terminal.is_running();
        let mut output = if terminal.last_run.is_some() {
            capture.output
        } else {
            raw
        };
        truncate_output_for_display(&mut output, self.max_output_bytes);

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

    async fn refresh_terminal_completion(
        &self,
        terminal: ManagedTerminal,
    ) -> Result<ManagedTerminal> {
        let Some(run) = terminal.last_run.as_ref() else {
            return Ok(terminal);
        };
        if run.completed || !run.marker_enabled {
            return Ok(terminal);
        }
        let raw = self.capture_pane(&terminal, MAX_CAPTURE_LINES).await?;
        let capture = extract_run_capture(&raw, terminal.last_run.as_ref());
        if !capture.completed {
            return Ok(terminal);
        }
        self.mark_run_completed(&terminal.id, capture.exit_code)
            .await;
        let mut refreshed = terminal;
        if let Some(ref mut run) = refreshed.last_run {
            run.completed = true;
            run.exit_code = capture.exit_code;
        }
        Ok(refreshed)
    }

    async fn resolve_terminal(
        &self,
        session_key: &str,
        terminal_id: Option<&str>,
        force_new: bool,
        cwd: Option<&str>,
    ) -> Result<ManagedTerminal> {
        if let Some(terminal_id) = terminal_id.filter(|id| !id.trim().is_empty()) {
            return self.lookup_terminal(session_key, terminal_id).await;
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
        self.create_or_discover_terminal(session_key, force_new || has_busy_terminal, cwd)
            .await
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
            .ok_or_else(|| Error::message(format!("terminal id not found: {terminal_id}")))?;
        if terminal.session_key != session_key {
            return Err(Error::message(format!(
                "terminal {terminal_id} belongs to another session"
            )));
        }
        Ok(terminal)
    }

    async fn find_idle_terminal(&self, session_key: &str) -> Result<Option<ManagedTerminal>> {
        let terminals: Vec<ManagedTerminal> = self
            .terminals
            .read()
            .await
            .values()
            .filter(|terminal| terminal.session_key == session_key && !terminal.is_running())
            .cloned()
            .collect();
        for terminal in terminals {
            if self.pane_exists(session_key, &terminal.pane_id).await? {
                return Ok(Some(terminal));
            }
            self.terminals.write().await.remove(&terminal.id);
        }
        Ok(None)
    }

    async fn create_or_discover_terminal(
        &self,
        session_key: &str,
        force_new: bool,
        cwd: Option<&str>,
    ) -> Result<ManagedTerminal> {
        let mut panes = self.list_panes(session_key).await?;
        let mut created_session = false;
        if panes.is_empty() {
            let session_name = default_session_name(session_key);
            self.new_session(session_key, &session_name, cwd).await?;
            created_session = true;
            panes = self.list_panes(session_key).await?;
        }
        if panes.is_empty() {
            return Err(Error::message("tmux session has no panes"));
        }

        let pane = if force_new && !created_session {
            let session = choose_active_pane(&panes)
                .ok_or_else(|| Error::message("tmux session has no active pane"))?;
            self.new_window(session_key, &session.session_id, cwd)
                .await?
        } else {
            choose_active_pane(&panes)
                .ok_or_else(|| Error::message("tmux session has no active pane"))?
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

    async fn pane_exists(&self, session_key: &str, pane_id: &str) -> Result<bool> {
        Ok(self
            .list_panes(session_key)
            .await?
            .into_iter()
            .any(|pane| pane.pane_id == pane_id))
    }

    async fn new_session(
        &self,
        session_key: &str,
        session_name: &str,
        cwd: Option<&str>,
    ) -> Result<()> {
        let cwd = cwd.unwrap_or(SANDBOX_WORKDIR);
        let args = vec![
            "new-session".to_string(),
            "-d".to_string(),
            "-s".to_string(),
            session_name.to_string(),
            "-x".to_string(),
            DEFAULT_TMUX_COLS.to_string(),
            "-y".to_string(),
            DEFAULT_TMUX_ROWS.to_string(),
            "-c".to_string(),
            cwd.to_string(),
            "bash -l".to_string(),
        ];
        let output = self
            .run_tmux(session_key, &args, Duration::from_secs(10))
            .await?;
        if output.exit_code != 0 {
            return Err(Error::message(format!(
                "failed to create tmux session: {}",
                command_error_text(&output)
            )));
        }
        Ok(())
    }

    async fn new_window(
        &self,
        session_key: &str,
        session_id: &str,
        cwd: Option<&str>,
    ) -> Result<TmuxPaneInfo> {
        let cwd = cwd.unwrap_or(SANDBOX_WORKDIR);
        let window_name = format!("term-{}", self.next_id.load(Ordering::Relaxed));
        let format = tmux_format(&[
            "session_id",
            "session_name",
            "window_id",
            "window_index",
            "window_name",
            "window_active",
            "pane_id",
            "pane_index",
            "pane_active",
        ]);
        let args = vec![
            "new-window".to_string(),
            "-d".to_string(),
            "-P".to_string(),
            "-F".to_string(),
            format,
            "-t".to_string(),
            session_id.to_string(),
            "-n".to_string(),
            window_name,
            "-c".to_string(),
            cwd.to_string(),
            "bash -l".to_string(),
        ];
        let output = self
            .run_tmux(session_key, &args, Duration::from_secs(10))
            .await?;
        if output.exit_code != 0 {
            return Err(Error::message(format!(
                "failed to create tmux window: {}",
                command_error_text(&output)
            )));
        }
        parse_pane_line(output.stdout.trim()).ok_or_else(|| {
            Error::message(format!(
                "failed to parse tmux new-window output: {}",
                output.stdout.trim()
            ))
        })
    }

    async fn list_panes(&self, session_key: &str) -> Result<Vec<TmuxPaneInfo>> {
        let format = tmux_format(&[
            "session_id",
            "session_name",
            "window_id",
            "window_index",
            "window_name",
            "window_active",
            "pane_id",
            "pane_index",
            "pane_active",
        ]);
        let args = vec!["list-panes".to_string(), "-aF".to_string(), format];
        let output = self
            .run_tmux(session_key, &args, Duration::from_secs(10))
            .await?;
        if output.exit_code != 0 {
            let message = command_error_text(&output);
            if is_tmux_no_server(&message) {
                return Ok(Vec::new());
            }
            if is_tmux_missing(&message) {
                return Err(Error::message("tmux is not installed in the sandbox"));
            }
            return Err(Error::message(format!(
                "failed to list tmux panes: {message}"
            )));
        }
        output
            .stdout
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                parse_pane_line(line)
                    .ok_or_else(|| Error::message(format!("invalid tmux pane line: {line}")))
            })
            .collect()
    }

    async fn capture_pane(&self, terminal: &ManagedTerminal, max_lines: usize) -> Result<String> {
        let args = vec![
            "capture-pane".to_string(),
            "-t".to_string(),
            terminal.pane_id.clone(),
            "-p".to_string(),
            "-S".to_string(),
            format!("-{}", max_lines.clamp(1, MAX_CAPTURE_LINES)),
        ];
        let output = self
            .run_tmux(&terminal.session_key, &args, Duration::from_secs(10))
            .await?;
        if output.exit_code != 0 {
            return Err(Error::message(format!(
                "failed to capture terminal output: {}",
                command_error_text(&output)
            )));
        }
        Ok(output.stdout)
    }

    async fn paste_text(&self, terminal: &ManagedTerminal, text: &str) -> Result<()> {
        let buffer_name = tmux_paste_buffer_name(&terminal.id);
        let set_args = set_paste_buffer_args(&buffer_name, text);
        let set_output = self
            .run_tmux(&terminal.session_key, &set_args, Duration::from_secs(10))
            .await?;
        if set_output.exit_code != 0 {
            return Err(Error::message(format!(
                "failed to set tmux paste buffer: {}",
                command_error_text(&set_output)
            )));
        }

        let paste_args = paste_buffer_args(&buffer_name, &terminal.pane_id);
        let paste_output = match self
            .run_tmux(&terminal.session_key, &paste_args, Duration::from_secs(10))
            .await
        {
            Ok(output) => output,
            Err(err) => {
                self.delete_paste_buffer(terminal, &buffer_name).await;
                return Err(err);
            },
        };
        if paste_output.exit_code != 0 {
            self.delete_paste_buffer(terminal, &buffer_name).await;
            return Err(Error::message(format!(
                "failed to paste command into tmux pane: {}",
                command_error_text(&paste_output)
            )));
        }
        Ok(())
    }

    async fn delete_paste_buffer(&self, terminal: &ManagedTerminal, buffer_name: &str) {
        let delete_args = vec![
            "delete-buffer".to_string(),
            "-b".to_string(),
            buffer_name.to_string(),
        ];
        if let Err(err) = self
            .run_tmux(&terminal.session_key, &delete_args, Duration::from_secs(10))
            .await
        {
            debug!(?err, buffer_name, "failed to clean up tmux paste buffer");
        }
    }

    async fn run_tmux(
        &self,
        session_key: &str,
        tmux_args: &[String],
        timeout: Duration,
    ) -> Result<ExecResult> {
        let backend = self.sandbox_router.resolve_backend(session_key).await;
        if !backend.provides_fs_isolation() || !self.sandbox_router.is_sandboxed(session_key).await
        {
            return Err(Error::message(
                "tmux-backed execute_command requires an isolated sandbox session",
            ));
        }

        let id = self.sandbox_router.sandbox_id_for(session_key);
        let image = self
            .sandbox_router
            .resolve_image_for_backend_nowait(session_key, None, backend.backend_name())
            .await;
        backend.ensure_ready(&id, Some(&image)).await?;

        let mut words = Vec::with_capacity(tmux_args.len() + 1);
        words.push("tmux".to_string());
        words.extend(tmux_args.iter().cloned());
        let command = shell_words::join(words);
        let opts = ExecOpts {
            timeout,
            max_output_bytes: self.max_output_bytes,
            working_dir: Some(SANDBOX_WORKDIR.into()),
            env: Vec::new(),
        };
        debug!(session = session_key, command, "sandbox tmux command");
        backend.exec(&id, &command, &opts).await
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

pub struct ExecuteCommandTool {
    manager: Arc<TmuxTerminalManager>,
}

impl ExecuteCommandTool {
    #[must_use]
    pub fn new(manager: Arc<TmuxTerminalManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl AgentTool for ExecuteCommandTool {
    fn name(&self) -> &str {
        "execute_command"
    }

    fn description(&self) -> &str {
        "Execute a shell command by pasting it into a real tmux terminal inside the current sandbox. Returns terminalId for follow-up read_terminal_output calls."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "Shell command to execute in the sandbox tmux terminal"
                },
                "customCwd": {
                    "type": "string",
                    "description": "Working directory inside the sandbox terminal"
                },
                "newTerminal": {
                    "type": "boolean",
                    "description": "If true, create a new tmux window/terminal for this command"
                },
                "destructiveFlag": {
                    "type": "boolean",
                    "description": "Compatibility hint for command approval UIs"
                },
                "background": {
                    "type": "boolean",
                    "description": "If true, start the command and return immediately without waiting for completion"
                },
                "timeout": {
                    "type": "integer",
                    "description": "Milliseconds to wait for completion before returning partial output"
                },
                "terminalId": {
                    "type": "string",
                    "description": "Managed tmux terminal id returned by a previous execute_command call"
                }
            },
            "required": ["command"]
        })
    }

    async fn execute(&self, params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        let parsed: ExecuteCommandParams = serde_json::from_value(without_null_params(params))?;
        let session_key = parsed._session_key.as_deref().unwrap_or("main").to_string();
        info!(session = session_key, "execute_command tmux tool invoked");
        Ok(serde_json::to_value(
            self.manager.execute_command(&session_key, parsed).await?,
        )?)
    }
}

pub struct ReadTerminalOutputTool {
    manager: Arc<TmuxTerminalManager>,
}

impl ReadTerminalOutputTool {
    #[must_use]
    pub fn new(manager: Arc<TmuxTerminalManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl AgentTool for ReadTerminalOutputTool {
    fn name(&self) -> &str {
        "read_terminal_output"
    }

    fn description(&self) -> &str {
        "Read current output from a managed sandbox tmux terminal created by execute_command."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "terminalId": {
                    "type": "string",
                    "description": "Managed terminal id returned by execute_command"
                },
                "maxLines": {
                    "type": "integer",
                    "description": "Maximum number of tmux scrollback lines to read"
                }
            },
            "required": ["terminalId"]
        })
    }

    async fn execute(&self, params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        let parsed: ReadTerminalOutputParams = serde_json::from_value(without_null_params(params))?;
        let session_key = parsed._session_key.as_deref().unwrap_or("main").to_string();
        Ok(serde_json::to_value(
            self.manager
                .read_terminal_output(&session_key, parsed)
                .await?,
        )?)
    }
}

fn tmux_paste_buffer_name(terminal_id: &str) -> String {
    let id = terminal_id
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
        .collect::<String>();
    format!("moltis-paste-{}-{}", id, uuid::Uuid::new_v4().simple())
}

fn set_paste_buffer_args(buffer_name: &str, text: &str) -> Vec<String> {
    vec![
        "set-buffer".to_string(),
        "-b".to_string(),
        buffer_name.to_string(),
        "--".to_string(),
        text.to_string(),
    ]
}

fn paste_buffer_args(buffer_name: &str, pane_id: &str) -> Vec<String> {
    vec![
        "paste-buffer".to_string(),
        "-d".to_string(),
        "-b".to_string(),
        buffer_name.to_string(),
        "-t".to_string(),
        pane_id.to_string(),
    ]
}

fn build_paste_payload(
    command: &str,
    cwd: Option<&str>,
    run_id: &str,
    marker_enabled: bool,
) -> String {
    let mut statements = Vec::new();
    if let Some(cwd) = cwd.filter(|value| !value.trim().is_empty()) {
        statements.push(format!("cd {}", shell_words::quote(cwd).as_ref()));
    }
    if marker_enabled {
        statements.push(format!("printf '\\n{START_PREFIX}{run_id}\\n'"));
        statements.push(format!("eval {}", shell_words::quote(command).as_ref()));
        statements.push("__moltis_exit=$?".to_string());
        statements.push(format!(
            "printf '\\n{DONE_PREFIX}{run_id}:%s\\n' \"$__moltis_exit\""
        ));
        let mut payload = statements.join("; ");
        payload.push('\n');
        return payload;
    }
    statements.push(command.to_string());
    let mut payload = statements.join("\n");
    payload.push('\n');
    payload
}

fn extract_run_capture(raw: &str, run: Option<&ManagedRun>) -> CaptureResult {
    let Some(run) = run else {
        return CaptureResult {
            output: raw.to_string(),
            completed: false,
            exit_code: None,
        };
    };
    let lines: Vec<&str> = raw.lines().collect();
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
    let mut output_lines = Vec::new();
    let mut completed = run.completed;
    let mut exit_code = run.exit_code;

    for line in lines.iter().skip(start) {
        if let Some(raw_code) = line.trim().strip_prefix(&done_marker) {
            exit_code = raw_code.trim().parse::<i32>().ok();
            completed = true;
            break;
        }
        if should_skip_wrapper_line(line, run) {
            continue;
        }
        output_lines.push(*line);
    }

    CaptureResult {
        output: output_lines.join("\n").trim().to_string(),
        completed,
        exit_code,
    }
}

fn should_skip_wrapper_line(line: &str, run: &ManagedRun) -> bool {
    let trimmed = line.trim();
    trimmed.is_empty()
        || trimmed == run.command.trim()
        || trimmed.ends_with(run.command.trim()) && prompt_prefix(trimmed, run.command.trim())
        || trimmed.contains(START_PREFIX)
        || trimmed.contains("__moltis_exit=$?")
        || trimmed.contains(DONE_PREFIX)
}

fn prompt_prefix(line: &str, command: &str) -> bool {
    let prefix = line.trim_end_matches(command).trim_end();
    prefix
        .chars()
        .last()
        .is_some_and(|last| matches!(last, '$' | '#' | '>' | '%'))
}

fn tmux_format(fields: &[&str]) -> String {
    fields
        .iter()
        .map(|field| format!("#{{{field}}}"))
        .collect::<Vec<_>>()
        .join(FIELD_SEP)
}

fn parse_pane_line(line: &str) -> Option<TmuxPaneInfo> {
    let parts: Vec<&str> = line.split(FIELD_SEP).collect();
    if parts.len() != 9 {
        return None;
    }
    Some(TmuxPaneInfo {
        session_id: parts[0].to_string(),
        session_name: parts[1].to_string(),
        window_id: parts[2].to_string(),
        window_index: parts[3].parse().ok()?,
        window_name: parts[4].to_string(),
        window_active: parse_bool_flag(parts[5]),
        pane_id: parts[6].to_string(),
        pane_index: parts[7].parse().ok()?,
        pane_active: parse_bool_flag(parts[8]),
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

fn parse_bool_flag(raw: &str) -> bool {
    raw.trim() == "1"
}

fn default_session_name(session_key: &str) -> String {
    let mut name = String::from("moltis-");
    for ch in session_key.chars().take(48) {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
            name.push(ch);
        } else {
            name.push('-');
        }
    }
    if name == "moltis-" {
        name.push_str("main");
    }
    name
}

fn command_error_text(output: &ExecResult) -> String {
    let stderr = output.stderr.trim();
    if !stderr.is_empty() {
        return stderr.to_string();
    }
    let stdout = output.stdout.trim();
    if !stdout.is_empty() {
        return stdout.to_string();
    }
    format!("exit {}", output.exit_code)
}

fn is_tmux_no_server(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("no server running")
        || lower.contains("no sessions")
        || lower.contains("error connecting to")
            && lower.contains("/tmux-")
            && lower.contains("no such file")
}

fn is_tmux_missing(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("tmux: command not found")
        || lower.contains("executable file not found")
        || lower.contains("tmux: not found")
        || lower.contains("command not found")
}

fn truncate_output_for_display(output: &mut String, max_output_bytes: usize) {
    if output.len() <= max_output_bytes {
        return;
    }
    output.truncate(output.floor_char_boundary(max_output_bytes));
    output.push_str("\n... [output truncated]");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tmux_paste_buffer_name_is_unique_and_tmux_safe() {
        let first = tmux_paste_buffer_name("term/1:%2");
        let second = tmux_paste_buffer_name("term/1:%2");

        assert_ne!(first, second);
        assert!(first.starts_with("moltis-paste-term12-"));
        assert!(
            first
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
        );
    }

    #[test]
    fn paste_buffer_args_use_named_delete_after_paste_buffer() {
        let set_args = set_paste_buffer_args("moltis-paste-1-abc", "cat <<'EOF'\nhello\nEOF\n");
        let paste_args = paste_buffer_args("moltis-paste-1-abc", "%2");

        assert_eq!(set_args, vec![
            "set-buffer",
            "-b",
            "moltis-paste-1-abc",
            "--",
            "cat <<'EOF'\nhello\nEOF\n"
        ]);
        assert_eq!(paste_args, vec![
            "paste-buffer",
            "-d",
            "-b",
            "moltis-paste-1-abc",
            "-t",
            "%2"
        ]);
    }

    #[test]
    fn paste_payload_adds_marker_for_foreground_commands() {
        let payload = build_paste_payload("echo true", Some("/home/sandbox"), "abc", true);
        assert!(payload.contains("cd /home/sandbox"));
        assert!(payload.contains("__MOLTIS_EXEC_START__abc"));
        assert!(payload.contains("eval 'echo true'"));
        assert!(payload.contains("__MOLTIS_EXEC_DONE__abc"));
    }

    #[test]
    fn paste_payload_keeps_done_marker_in_same_shell_input_as_command() {
        let payload = build_paste_payload(
            "apt-get update -qq && apt-get install -y -qq iputils-ping",
            Some("/home/sandbox"),
            "abc",
            true,
        );

        assert_eq!(payload.lines().count(), 1);
        assert!(
            payload
                .contains("; eval 'apt-get update -qq && apt-get install -y -qq iputils-ping'; ")
        );
        assert!(payload.contains("; __moltis_exit=$?; "));
        assert!(payload.contains("__MOLTIS_EXEC_DONE__abc:%s"));
    }

    #[test]
    fn paste_payload_can_omit_marker_when_requested() {
        let payload = build_paste_payload("sleep 100", None, "abc", false);
        assert!(payload.contains("sleep 100"));
        assert!(!payload.contains("__MOLTIS_EXEC_DONE__abc"));
    }

    #[test]
    fn extract_run_capture_returns_command_output_and_exit_code() {
        let run = ManagedRun {
            run_id: "abc".to_string(),
            command: "echo true".to_string(),
            baseline_lines: 2,
            marker_enabled: true,
            completed: false,
            exit_code: None,
        };
        let raw = "old\nold2\n$ printf '\\n__MOLTIS_EXEC_START__abc\\n'\n__MOLTIS_EXEC_START__abc\n$ echo true\ntrue\n__moltis_exit=$?\nprintf '\\n__MOLTIS_EXEC_DONE__abc:%s\\n' \"$__moltis_exit\"\n__MOLTIS_EXEC_DONE__abc:0\n$ ";
        let capture = extract_run_capture(raw, Some(&run));
        assert!(capture.completed);
        assert_eq!(capture.exit_code, Some(0));
        assert_eq!(capture.output, "true");
    }

    #[test]
    fn extract_run_capture_uses_tail_when_baseline_scrollback_is_truncated() {
        let run = ManagedRun {
            run_id: "abc".to_string(),
            command: "yes".to_string(),
            baseline_lines: 20_000,
            marker_enabled: true,
            completed: false,
            exit_code: None,
        };
        let raw = "line one\nline two\nline three";
        let capture = extract_run_capture(raw, Some(&run));
        assert!(!capture.completed);
        assert_eq!(capture.output, raw);
    }

    #[test]
    fn parse_pane_line_reads_tmux_ids() {
        let sep = FIELD_SEP;
        let line = format!("$0{sep}main{sep}@1{sep}0{sep}bash{sep}1{sep}%2{sep}0{sep}1");
        let pane = parse_pane_line(&line).expect("pane line should parse");
        assert_eq!(pane.session_id, "$0");
        assert_eq!(pane.session_name, "main");
        assert_eq!(pane.window_id, "@1");
        assert_eq!(pane.pane_id, "%2");
        assert!(pane.window_active);
        assert!(pane.pane_active);
    }

    #[test]
    fn tmux_socket_missing_is_no_server_not_missing_binary() {
        let message = "error connecting to /tmp/tmux-0/default (No such file or directory)";

        assert!(is_tmux_no_server(message));
        assert!(!is_tmux_missing(message));
    }

    #[test]
    fn tmux_command_not_found_is_missing_binary() {
        let message = "bash: line 1: tmux: command not found";

        assert!(!is_tmux_no_server(message));
        assert!(is_tmux_missing(message));
    }
}
