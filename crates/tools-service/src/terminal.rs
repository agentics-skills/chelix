use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use {
    anyhow::{Context, Result, anyhow, bail},
    chelix_protocol::{
        ExecuteCommandRequest, ExecuteCommandResponse, ReadTerminalOutputRequest,
        ReadTerminalOutputResponse, ToolsServiceTerminalInfo,
    },
    rmux_core::{
        KEYC_META, Screen, TerminalScreen, key_code_to_bytes, key_string_lookup_key,
        key_string_lookup_string,
    },
    rmux_pty::{ChildCommand, PtyChild, PtyIo, PtyMaster, Signal, TerminalSize},
    sha2::{Digest, Sha256},
    tokio::sync::{Notify, RwLock},
    uuid::Uuid,
};

const DEFAULT_COLS: u16 = 220;
const DEFAULT_ROWS: u16 = 56;
const MAX_COMMAND_BYTES: usize = 1024 * 1024;
const MAX_INPUT_BYTES: usize = 1024 * 1024;
const PROMPT_COMMAND: &str = r#"__chelix_status=$?; printf '\033]633;D;%s\007' "$__chelix_status"; trap 'trap - DEBUG; printf "\033]633;C\007"' DEBUG"#;

pub(crate) struct TerminalManager {
    default_working_dir: PathBuf,
    next_terminal_id: AtomicU64,
    terminals: RwLock<HashMap<String, Arc<ManagedTerminal>>>,
}

pub(crate) struct TerminalSubscription {
    pub(crate) initial_output: Vec<u8>,
    terminal: Arc<ManagedTerminal>,
    offset: usize,
}

struct ManagedTerminal {
    id: String,
    session_key: String,
    writer: Mutex<PtyMaster>,
    child: Mutex<Option<PtyChild>>,
    environment_fingerprint: [u8; 32],
    output: Mutex<TerminalOutput>,
    output_notify: Notify,
}

struct TerminalOutput {
    history: Vec<u8>,
    screen: TerminalScreen,
    parser: ShellEventParser,
    active_run: Option<ManagedRun>,
    last_exit_code: Option<i32>,
    ready: bool,
    at_prompt: bool,
    closed: bool,
}

struct ManagedRun {
    id: String,
    submission_line: usize,
    output_start_line: Option<usize>,
    output_end_line: Option<usize>,
    exit_code: Option<i32>,
    completed: bool,
}

#[derive(Default)]
struct ShellEventParser {
    pending: Vec<u8>,
}

#[derive(Debug, PartialEq, Eq)]
struct ParsedOutput {
    items: Vec<ParsedOutputItem>,
}

#[derive(Debug, PartialEq, Eq)]
enum ParsedOutputItem {
    Output(Vec<u8>),
    CommandStarted,
    CommandFinished(i32),
}

impl TerminalManager {
    pub(crate) fn new(default_working_dir: PathBuf) -> Result<Self> {
        if !default_working_dir.is_dir() {
            bail!(
                "terminal working directory is unavailable: {}",
                default_working_dir.display()
            );
        }
        Ok(Self {
            default_working_dir,
            next_terminal_id: AtomicU64::new(1),
            terminals: RwLock::new(HashMap::new()),
        })
    }

    pub(crate) async fn execute_command(
        &self,
        request: ExecuteCommandRequest,
    ) -> Result<ExecuteCommandResponse> {
        validate_execute_request(&request)?;
        let terminal = self.resolve_terminal(&request).await?;
        ensure_terminal_alive(&terminal)?;
        ensure_environment_matches(&terminal, &request.env)?;
        wait_for_terminal_ready(
            &terminal,
            Duration::from_millis(request.timeout_millis.max(1)),
        )
        .await?;

        let run_id = Uuid::new_v4().simple().to_string();
        let command = build_command_input(&request)?;
        let submission_line = {
            let mut output = lock(&terminal.output);
            if output.active_run.as_ref().is_some_and(|run| !run.completed) {
                bail!(
                    "terminal {} is still running a command; use read_terminal_output or newTerminal=true",
                    terminal.id
                );
            }
            if !output.at_prompt {
                bail!(
                    "terminal {} is not at a shell prompt; wait for the current interactive command to finish",
                    terminal.id
                );
            }
            let submission_line = output.screen.screen().cursor_absolute_y();
            output.active_run = Some(ManagedRun {
                id: run_id.clone(),
                submission_line,
                output_start_line: None,
                output_end_line: None,
                exit_code: None,
                completed: false,
            });
            output.at_prompt = false;
            submission_line
        };

        if let Err(error) = write_command(&terminal, &command) {
            let mut output = lock(&terminal.output);
            output.active_run = None;
            output.at_prompt = true;
            terminal.output_notify.notify_waiters();
            return Err(error);
        }

        if request.background {
            return response_for_run(&terminal, &run_id, submission_line, false, false, true);
        }

        let completed = wait_for_run(
            &terminal,
            &run_id,
            Duration::from_millis(request.timeout_millis),
        )
        .await?;
        response_for_run(
            &terminal,
            &run_id,
            submission_line,
            completed,
            !completed,
            false,
        )
    }

    pub(crate) async fn read_terminal_output(
        &self,
        request: ReadTerminalOutputRequest,
    ) -> Result<ReadTerminalOutputResponse> {
        let terminal = self
            .find_terminal(&request.session_key, &request.terminal_id)
            .await?;
        let output = lock(&terminal.output);
        let running = !output.closed && !output.at_prompt;
        let completed = !running;
        let text = retained_text(output.screen.screen(), request.max_lines)?;
        Ok(ReadTerminalOutputResponse {
            terminal_id: terminal.id.clone(),
            output: text,
            exit_code: output.last_exit_code,
            completed,
            running,
            alive: !output.closed,
        })
    }

    pub(crate) async fn create_interactive_terminal(
        &self,
        session_key: &str,
        environment: &[chelix_protocol::ToolsServiceEnvVar],
    ) -> Result<ToolsServiceTerminalInfo> {
        validate_session_key(session_key)?;
        let terminal = self.spawn_terminal(session_key, &self.default_working_dir, environment)?;
        let info = terminal_info(&terminal);
        self.terminals
            .write()
            .await
            .insert(terminal.id.clone(), terminal);
        Ok(info)
    }

    pub(crate) async fn terminal_info(
        &self,
        session_key: &str,
        terminal_id: &str,
    ) -> Result<ToolsServiceTerminalInfo> {
        self.find_terminal(session_key, terminal_id)
            .await
            .map(|terminal| terminal_info(&terminal))
    }

    pub(crate) async fn terminal_infos(&self) -> Result<Vec<ToolsServiceTerminalInfo>> {
        let infos = {
            let terminals = self.terminals.read().await;
            terminals.values().map(terminal_info).collect()
        };
        order_terminal_infos(infos)
    }

    pub(crate) async fn terminal_ids(&self, session_key: &str) -> Result<Vec<String>> {
        validate_session_key(session_key)?;
        let terminals = self.terminals.read().await;
        let mut ids = terminals
            .values()
            .filter(|terminal| terminal.session_key == session_key)
            .map(|terminal| terminal.id.clone())
            .collect::<Vec<_>>();
        ids.sort();
        Ok(ids)
    }

    pub(crate) async fn send_terminal_keys(
        &self,
        session_key: &str,
        terminal_id: &str,
        keys: &str,
    ) -> Result<()> {
        let bytes = encode_keys(keys)?;
        self.write_terminal(session_key, terminal_id, &bytes).await
    }

    pub(crate) async fn paste_terminal_text(
        &self,
        session_key: &str,
        terminal_id: &str,
        text: &str,
    ) -> Result<()> {
        self.write_terminal(session_key, terminal_id, text.as_bytes())
            .await
    }

    pub(crate) async fn write_terminal(
        &self,
        session_key: &str,
        terminal_id: &str,
        bytes: &[u8],
    ) -> Result<()> {
        if bytes.len() > MAX_INPUT_BYTES {
            bail!("terminal input exceeds {MAX_INPUT_BYTES} bytes");
        }
        let terminal = self.find_terminal(session_key, terminal_id).await?;
        ensure_terminal_alive(&terminal)?;
        record_terminal_input(&terminal, bytes);
        lock(&terminal.writer)
            .write_all(bytes)
            .context("writing terminal input")
    }

    pub(crate) async fn resize_terminal(
        &self,
        session_key: &str,
        terminal_id: &str,
        cols: u16,
        rows: u16,
    ) -> Result<()> {
        let terminal = self.find_terminal(session_key, terminal_id).await?;
        let size = TerminalSize::new(cols.max(2), rows.max(1));
        lock(&terminal.writer)
            .resize(size)
            .context("resizing terminal PTY")?;
        lock(&terminal.output).screen.resize(size);
        Ok(())
    }

    pub(crate) async fn subscribe_terminal(
        &self,
        session_key: &str,
        terminal_id: &str,
    ) -> Result<TerminalSubscription> {
        let terminal = self.find_terminal(session_key, terminal_id).await?;
        let (initial_output, offset) = {
            let output = lock(&terminal.output);
            (output.history.clone(), output.history.len())
        };
        Ok(TerminalSubscription {
            initial_output,
            terminal,
            offset,
        })
    }

    pub(crate) async fn kill_terminal(&self, session_key: &str, terminal_id: &str) -> Result<()> {
        let terminal = self.find_terminal(session_key, terminal_id).await?;
        terminate_terminal(&terminal)?;
        self.terminals.write().await.remove(terminal_id);
        Ok(())
    }

    pub(crate) async fn shutdown(&self) -> Result<()> {
        let terminals = {
            let mut terminals = self.terminals.write().await;
            terminals
                .drain()
                .map(|(_, terminal)| terminal)
                .collect::<Vec<_>>()
        };
        let errors = terminals
            .iter()
            .filter_map(|terminal| terminate_terminal(terminal).err())
            .map(|error| error.to_string())
            .collect::<Vec<_>>();
        if errors.is_empty() {
            Ok(())
        } else {
            bail!("terminal shutdown failed: {}", errors.join("; "))
        }
    }

    async fn resolve_terminal(
        &self,
        request: &ExecuteCommandRequest,
    ) -> Result<Arc<ManagedTerminal>> {
        if request.new_terminal {
            if request.terminal_id.is_some() {
                bail!("terminalId cannot be combined with newTerminal=true");
            }
            return self.create_for_request(request).await;
        }
        if let Some(terminal_id) = request.terminal_id.as_deref() {
            return self.find_terminal(&request.session_key, terminal_id).await;
        }
        let requested_environment = validated_environment(&request.env)?;
        let requested_fingerprint = environment_fingerprint(&requested_environment);
        let matching = self
            .terminals
            .read()
            .await
            .values()
            .filter(|terminal| {
                terminal.session_key == request.session_key
                    && terminal.environment_fingerprint == requested_fingerprint
                    && !lock(&terminal.output).closed
            })
            .cloned()
            .collect::<Vec<_>>();
        match matching.as_slice() {
            [] => self.create_for_request(request).await,
            [terminal] => Ok(Arc::clone(terminal)),
            _ => bail!(
                "multiple terminals match this session and environment; provide terminalId explicitly"
            ),
        }
    }

    async fn create_for_request(
        &self,
        request: &ExecuteCommandRequest,
    ) -> Result<Arc<ManagedTerminal>> {
        let cwd = request
            .custom_cwd
            .as_deref()
            .map(Path::new)
            .unwrap_or(&self.default_working_dir);
        validate_working_dir(cwd)?;
        let terminal = self.spawn_terminal(&request.session_key, cwd, &request.env)?;
        self.terminals
            .write()
            .await
            .insert(terminal.id.clone(), Arc::clone(&terminal));
        Ok(terminal)
    }

    fn spawn_terminal(
        &self,
        session_key: &str,
        cwd: &Path,
        environment: &[chelix_protocol::ToolsServiceEnvVar],
    ) -> Result<Arc<ManagedTerminal>> {
        validate_session_key(session_key)?;
        validate_working_dir(cwd)?;
        let environment = validated_environment(environment)?;
        let environment_fingerprint = environment_fingerprint(&environment);
        let command = environment.iter().fold(
            ChildCommand::new("/bin/bash")
                .args(["--noprofile", "--norc", "-i"])
                .current_dir(cwd)
                .size(TerminalSize::new(DEFAULT_COLS, DEFAULT_ROWS))
                .env("TERM", "xterm-256color")
                .env("COLORTERM", "truecolor")
                .env("HISTCONTROL", "ignorespace")
                .env("PROMPT_COMMAND", PROMPT_COMMAND)
                .env("PS0", ""),
            |command, (key, value)| command.env(key, value),
        );
        let spawned = command.spawn().context("spawning RMUX terminal shell")?;
        let (mut master, child) = spawned.into_parts();
        let reader = master
            .try_clone_for_startup_reader()
            .context("cloning RMUX terminal reader")?
            .into_io();
        let terminal = Arc::new(ManagedTerminal {
            id: self
                .next_terminal_id
                .fetch_add(1, Ordering::Relaxed)
                .to_string(),
            session_key: session_key.to_owned(),
            writer: Mutex::new(master),
            child: Mutex::new(Some(child)),
            environment_fingerprint,
            output: Mutex::new(TerminalOutput {
                history: Vec::new(),
                screen: TerminalScreen::new(
                    TerminalSize::new(DEFAULT_COLS, DEFAULT_ROWS),
                    usize::MAX,
                ),
                parser: ShellEventParser::default(),
                active_run: None,
                last_exit_code: None,
                ready: false,
                at_prompt: false,
                closed: false,
            }),
            output_notify: Notify::new(),
        });
        spawn_output_reader(Arc::clone(&terminal), reader)?;
        Ok(terminal)
    }

    async fn find_terminal(
        &self,
        session_key: &str,
        terminal_id: &str,
    ) -> Result<Arc<ManagedTerminal>> {
        validate_session_key(session_key)?;
        let terminal = self
            .terminals
            .read()
            .await
            .get(terminal_id)
            .cloned()
            .ok_or_else(|| anyhow!("terminal {terminal_id} was not found"))?;
        if terminal.session_key != session_key {
            bail!("terminal {terminal_id} does not belong to this session");
        }
        Ok(terminal)
    }
}

impl TerminalSubscription {
    pub(crate) async fn next_output(&mut self) -> Result<Option<Vec<u8>>> {
        loop {
            let notified = self.terminal.output_notify.notified();
            {
                let output = lock(&self.terminal.output);
                if self.offset < output.history.len() {
                    let bytes = output.history[self.offset..].to_vec();
                    self.offset = output.history.len();
                    return Ok(Some(bytes));
                }
                if output.closed {
                    return Ok(None);
                }
            }
            notified.await;
        }
    }
}

fn spawn_output_reader(terminal: Arc<ManagedTerminal>, reader: PtyIo) -> Result<()> {
    std::thread::Builder::new()
        .name(format!("chelix-rmux-{}", terminal.id))
        .spawn(move || {
            reader.release_startup_slave_guard();
            let mut buffer = vec![0_u8; 16 * 1024];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => {
                        close_terminal_output(&terminal);
                        return;
                    },
                    Ok(count) => process_output(&terminal, &buffer[..count]),
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {},
                    Err(error) => {
                        tracing::error!(terminal_id = %terminal.id, %error, "RMUX terminal output failed");
                        close_terminal_output(&terminal);
                        return;
                    },
                }
            }
        })
        .context("starting RMUX terminal output reader")?;
    Ok(())
}

fn process_output(terminal: &ManagedTerminal, bytes: &[u8]) {
    let mut output = lock(&terminal.output);
    let parsed = output.parser.feed(bytes);
    for item in parsed.items {
        match item {
            ParsedOutputItem::Output(bytes) => {
                append_visible_output(terminal, &mut output, bytes);
            },
            ParsedOutputItem::CommandStarted => {
                output.at_prompt = false;
                let output_start_line = output.screen.screen().cursor_absolute_y();
                if let Some(run) = output.active_run.as_mut() {
                    run.output_start_line = Some(output_start_line);
                }
            },
            ParsedOutputItem::CommandFinished(exit_code) => {
                let was_ready = output.ready;
                output.ready = true;
                output.at_prompt = true;
                let output_end_line = output.screen.screen().cursor_absolute_y();
                if was_ready {
                    output.last_exit_code = Some(exit_code);
                }
                if let Some(run) = output.active_run.as_mut() {
                    run.output_end_line = Some(output_end_line);
                    run.exit_code = Some(exit_code);
                    run.completed = true;
                    output.last_exit_code = Some(exit_code);
                }
            },
        }
    }
    let replies = output.screen.take_replies();
    drop(output);
    if !replies.is_empty()
        && let Err(error) = lock(&terminal.writer).write_all(&replies)
    {
        tracing::error!(terminal_id = %terminal.id, %error, "writing RMUX terminal reply failed");
    }
    terminal.output_notify.notify_waiters();
}

fn append_visible_output(terminal: &ManagedTerminal, output: &mut TerminalOutput, bytes: Vec<u8>) {
    output.screen.feed(&bytes);
    output.history.extend_from_slice(&bytes);
    terminal.output_notify.notify_waiters();
}

fn close_terminal_output(terminal: &ManagedTerminal) {
    let pending = {
        let mut output = lock(&terminal.output);
        output.parser.finish()
    };
    if !pending.is_empty() {
        let mut output = lock(&terminal.output);
        append_visible_output(terminal, &mut output, pending);
    }
    let exit_code = lock(&terminal.child)
        .take()
        .and_then(|mut child| child.wait().ok())
        .and_then(|status| status.code());
    let mut output = lock(&terminal.output);
    output.closed = true;
    output.last_exit_code = exit_code;
    let output_end_line = output
        .screen
        .screen()
        .cursor_absolute_y()
        .saturating_add(1)
        .min(output.screen.screen().absolute_line_count());
    if let Some(run) = output.active_run.as_mut() {
        run.output_end_line = Some(output_end_line);
        run.completed = true;
        run.exit_code = exit_code;
    }
    drop(output);
    terminal.output_notify.notify_waiters();
}

async fn wait_for_run(terminal: &ManagedTerminal, run_id: &str, timeout: Duration) -> Result<bool> {
    let wait = async {
        loop {
            let notified = terminal.output_notify.notified();
            {
                let output = lock(&terminal.output);
                let run = output
                    .active_run
                    .as_ref()
                    .ok_or_else(|| anyhow!("terminal run {run_id} disappeared"))?;
                if run.id != run_id {
                    bail!("terminal run {run_id} was replaced before completion");
                }
                if run.completed || output.closed {
                    return Ok(true);
                }
            }
            notified.await;
        }
    };
    match tokio::time::timeout(timeout, wait).await {
        Ok(result) => result,
        Err(_) => Ok(false),
    }
}

async fn wait_for_terminal_ready(terminal: &ManagedTerminal, timeout: Duration) -> Result<()> {
    let wait = async {
        loop {
            let notified = terminal.output_notify.notified();
            {
                let output = lock(&terminal.output);
                if output.ready {
                    return Ok(());
                }
                if output.closed {
                    bail!(
                        "terminal {} exited before its shell became ready",
                        terminal.id
                    );
                }
            }
            notified.await;
        }
    };
    tokio::time::timeout(timeout, wait)
        .await
        .map_err(|_| anyhow!("terminal {} shell startup timed out", terminal.id))?
}

fn response_for_run(
    terminal: &ManagedTerminal,
    run_id: &str,
    submission_line: usize,
    completed: bool,
    timed_out: bool,
    background: bool,
) -> Result<ExecuteCommandResponse> {
    let output = lock(&terminal.output);
    let run = output
        .active_run
        .as_ref()
        .filter(|run| run.id == run_id)
        .ok_or_else(|| anyhow!("terminal run {run_id} is unavailable"))?;
    debug_assert_eq!(run.submission_line, submission_line);
    let command_output = match run.output_start_line {
        Some(start_line) => {
            let end_line = run.output_end_line.unwrap_or_else(|| {
                output
                    .screen
                    .screen()
                    .cursor_absolute_y()
                    .saturating_add(1)
                    .min(output.screen.screen().absolute_line_count())
            });
            screen_text(output.screen.screen(), start_line, end_line)?
                .trim_end_matches('\n')
                .to_owned()
        },
        None if run.completed => {
            bail!("terminal run {run_id} completed without a command-start boundary")
        },
        None => String::new(),
    };
    Ok(ExecuteCommandResponse {
        terminal_id: terminal.id.clone(),
        run_id: run_id.to_owned(),
        output: command_output,
        exit_code: run.exit_code,
        completed,
        alive: !output.closed,
        timed_out,
        background,
        message: if completed {
            format!("Command finished in terminal {}", terminal.id)
        } else if background {
            format!("Command started in terminal {}", terminal.id)
        } else {
            format!("Command is still running in terminal {}", terminal.id)
        },
    })
}

fn build_command_input(request: &ExecuteCommandRequest) -> Result<Vec<u8>> {
    let command = match request.custom_cwd.as_deref() {
        Some(cwd) => {
            validate_working_dir(Path::new(cwd))?;
            format!("builtin cd -- {} && {}", shell_quote(cwd)?, request.command)
        },
        None => request.command.clone(),
    };
    let mut input = Vec::with_capacity(command.len().saturating_mul(2).saturating_add(1));
    for byte in command.bytes() {
        if byte < 0x20 || byte == 0x7f {
            input.push(0x16);
        }
        input.push(byte);
    }
    input.push(b'\r');
    Ok(input)
}

fn write_command(terminal: &ManagedTerminal, command: &[u8]) -> Result<()> {
    lock(&terminal.writer)
        .write_all(command)
        .context("writing command to RMUX terminal")
}

fn record_terminal_input(terminal: &ManagedTerminal, bytes: &[u8]) {
    if bytes.iter().any(|byte| matches!(*byte, b'\r' | b'\n')) {
        lock(&terminal.output).at_prompt = false;
    }
}

fn terminate_terminal(terminal: &ManagedTerminal) -> Result<()> {
    let child = lock(&terminal.child);
    let Some(child) = child.as_ref() else {
        return Ok(());
    };
    let foreground_result = child.kill(Signal::HUP);
    let leader_result = child.kill_session_leader(Signal::HUP);
    if foreground_result.is_ok() || leader_result.is_ok() {
        return Ok(());
    }
    let foreground_error = foreground_result
        .err()
        .map(|error| error.to_string())
        .unwrap_or_else(|| "unknown foreground termination error".into());
    let leader_error = leader_result
        .err()
        .map(|error| error.to_string())
        .unwrap_or_else(|| "unknown session leader termination error".into());
    bail!(
        "terminating RMUX terminal failed: foreground: {foreground_error}; session leader: {leader_error}"
    )
}

fn ensure_terminal_alive(terminal: &ManagedTerminal) -> Result<()> {
    if lock(&terminal.output).closed {
        bail!("terminal {} has exited", terminal.id);
    }
    Ok(())
}

fn terminal_info(terminal: &Arc<ManagedTerminal>) -> ToolsServiceTerminalInfo {
    let output = lock(&terminal.output);
    let running = !output.closed && !output.at_prompt;
    ToolsServiceTerminalInfo {
        id: terminal.id.clone(),
        session_key: terminal.session_key.clone(),
        running,
        alive: !output.closed,
    }
}

fn order_terminal_infos(
    infos: Vec<ToolsServiceTerminalInfo>,
) -> Result<Vec<ToolsServiceTerminalInfo>> {
    let mut ordered = infos
        .into_iter()
        .map(|info| {
            let numeric_id = info
                .id
                .parse::<u64>()
                .with_context(|| format!("terminal id is not numeric: {:?}", info.id))?;
            Ok((numeric_id, info))
        })
        .collect::<Result<Vec<_>>>()?;
    ordered.sort_unstable_by_key(|(numeric_id, _)| *numeric_id);
    Ok(ordered.into_iter().map(|(_, info)| info).collect())
}

impl ShellEventParser {
    fn feed(&mut self, bytes: &[u8]) -> ParsedOutput {
        const PREFIX: &[u8] = b"\x1b]633;";
        self.pending.extend_from_slice(bytes);
        let mut items = Vec::new();
        loop {
            let Some(start) = find_bytes(&self.pending, PREFIX) else {
                let retain = partial_suffix_len(&self.pending, PREFIX);
                let visible_len = self.pending.len().saturating_sub(retain);
                if visible_len > 0 {
                    items.push(ParsedOutputItem::Output(
                        self.pending.drain(..visible_len).collect(),
                    ));
                }
                break;
            };
            if start > 0 {
                items.push(ParsedOutputItem::Output(
                    self.pending.drain(..start).collect(),
                ));
            }
            let Some(end) = self.pending[PREFIX.len()..]
                .iter()
                .position(|byte| *byte == 0x07)
                .map(|offset| PREFIX.len() + offset)
            else {
                break;
            };
            let payload = String::from_utf8_lossy(&self.pending[PREFIX.len()..end]);
            if payload == "C" {
                items.push(ParsedOutputItem::CommandStarted);
            } else if let Some(status) = payload.strip_prefix("D;")
                && let Ok(status) = status.parse::<i32>()
            {
                items.push(ParsedOutputItem::CommandFinished(status));
            }
            self.pending.drain(..=end);
        }
        ParsedOutput { items }
    }

    fn finish(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.pending)
    }
}

fn retained_text(screen: &Screen, max_lines: Option<usize>) -> Result<String> {
    let text = screen_text(screen, 0, screen.absolute_line_count())?
        .trim_end_matches('\n')
        .to_owned();
    Ok(limit_lines(text, max_lines))
}

fn screen_text(screen: &Screen, start_line: usize, end_line: usize) -> Result<String> {
    if start_line > end_line || end_line > screen.absolute_line_count() {
        bail!(
            "terminal screen range {start_line}..{end_line} is invalid for {} lines",
            screen.absolute_line_count()
        );
    }
    let mut output = String::new();
    for line_index in start_line..end_line {
        let line = screen
            .absolute_line_view(line_index)
            .ok_or_else(|| anyhow!("terminal screen line {line_index} is unavailable"))?;
        let mut text = String::new();
        for cell in line.cells() {
            if !cell.is_padding() {
                text.push_str(cell.text());
            }
        }
        if !line.wrapped() {
            text.truncate(text.trim_end_matches(' ').len());
        }
        output.push_str(&text);
        if !line.wrapped() {
            output.push('\n');
        }
    }
    Ok(output)
}

fn limit_lines(text: String, max_lines: Option<usize>) -> String {
    let Some(max_lines) = max_lines else {
        return text;
    };
    let lines = text.lines().collect::<Vec<_>>();
    let start = lines.len().saturating_sub(max_lines);
    lines[start..].join("\n")
}

fn shell_quote(value: &str) -> Result<String> {
    validate_no_nul(value, "shell value")?;
    Ok(format!("'{}'", value.replace('\'', "'\\''")))
}

fn validate_execute_request(request: &ExecuteCommandRequest) -> Result<()> {
    validate_session_key(&request.session_key)?;
    if request.command.is_empty() {
        bail!("command cannot be empty");
    }
    if request.command.len() > MAX_COMMAND_BYTES {
        bail!("command exceeds {MAX_COMMAND_BYTES} bytes");
    }
    validate_no_nul(&request.command, "command")?;
    if request.timeout_millis == 0 && !request.background {
        bail!("timeoutMillis must be greater than zero");
    }
    for variable in &request.env {
        validate_env_key(&variable.key)?;
        validate_no_nul(&variable.value, "environment value")?;
    }
    Ok(())
}

fn validated_environment(
    variables: &[chelix_protocol::ToolsServiceEnvVar],
) -> Result<BTreeMap<String, String>> {
    let mut environment = BTreeMap::new();
    for variable in variables {
        validate_env_key(&variable.key)?;
        validate_no_nul(&variable.value, "environment value")?;
        if environment
            .insert(variable.key.clone(), variable.value.clone())
            .is_some()
        {
            bail!("duplicate environment variable: {}", variable.key);
        }
    }
    Ok(environment)
}

fn environment_fingerprint(environment: &BTreeMap<String, String>) -> [u8; 32] {
    let mut hasher = Sha256::new();
    for (key, value) in environment {
        hasher.update(key.len().to_le_bytes());
        hasher.update(key.as_bytes());
        hasher.update(value.len().to_le_bytes());
        hasher.update(value.as_bytes());
    }
    hasher.finalize().into()
}

fn ensure_environment_matches(
    terminal: &ManagedTerminal,
    variables: &[chelix_protocol::ToolsServiceEnvVar],
) -> Result<()> {
    let requested = validated_environment(variables)?;
    if terminal.environment_fingerprint == environment_fingerprint(&requested) {
        return Ok(());
    }
    bail!(
        "terminal {} was created with a different environment; create a new terminal to apply changed variables",
        terminal.id
    )
}

fn validate_session_key(session_key: &str) -> Result<()> {
    if session_key.trim().is_empty() {
        bail!("session_key cannot be empty");
    }
    Ok(())
}

fn validate_working_dir(path: &Path) -> Result<()> {
    if !path.is_dir() {
        bail!(
            "terminal working directory is unavailable: {}",
            path.display()
        );
    }
    Ok(())
}

fn validate_env_key(key: &str) -> Result<()> {
    let mut chars = key.chars();
    let valid_start = chars
        .next()
        .is_some_and(|character| character == '_' || character.is_ascii_alphabetic());
    if !valid_start || !chars.all(|character| character == '_' || character.is_ascii_alphanumeric())
    {
        bail!("invalid environment variable name: {key}");
    }
    Ok(())
}

fn validate_no_nul(value: &str, field: &str) -> Result<()> {
    if value.contains('\0') {
        bail!("{field} cannot contain NUL bytes");
    }
    Ok(())
}

fn encode_keys(keys: &str) -> Result<Vec<u8>> {
    if keys.is_empty() {
        bail!("keys cannot be empty");
    }
    let Some(key) = key_string_lookup_string(keys) else {
        return Ok(keys.as_bytes().to_vec());
    };
    let canonical = key_string_lookup_key(key, false);
    if let Some(sequence) = named_key_sequence(&canonical) {
        return Ok(sequence.to_vec());
    }
    let mut bytes =
        key_code_to_bytes(key).ok_or_else(|| anyhow!("RMUX cannot encode terminal key {keys}"))?;
    if key & KEYC_META != 0 {
        bytes.insert(0, 0x1b);
    }
    Ok(bytes)
}

fn named_key_sequence(key: &str) -> Option<&'static [u8]> {
    match key {
        "Up" => Some(b"\x1b[A"),
        "Down" => Some(b"\x1b[B"),
        "Right" => Some(b"\x1b[C"),
        "Left" => Some(b"\x1b[D"),
        "Home" => Some(b"\x1b[H"),
        "End" => Some(b"\x1b[F"),
        "IC" => Some(b"\x1b[2~"),
        "DC" => Some(b"\x1b[3~"),
        "PPage" => Some(b"\x1b[5~"),
        "NPage" => Some(b"\x1b[6~"),
        "F1" => Some(b"\x1bOP"),
        "F2" => Some(b"\x1bOQ"),
        "F3" => Some(b"\x1bOR"),
        "F4" => Some(b"\x1bOS"),
        "F5" => Some(b"\x1b[15~"),
        "F6" => Some(b"\x1b[17~"),
        "F7" => Some(b"\x1b[18~"),
        "F8" => Some(b"\x1b[19~"),
        "F9" => Some(b"\x1b[20~"),
        "F10" => Some(b"\x1b[21~"),
        "F11" => Some(b"\x1b[23~"),
        "F12" => Some(b"\x1b[24~"),
        _ => None,
    }
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn partial_suffix_len(bytes: &[u8], prefix: &[u8]) -> usize {
    (1..prefix.len().min(bytes.len() + 1))
        .rev()
        .find(|length| bytes.ends_with(&prefix[..*length]))
        .unwrap_or(0)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command_request(session_key: &str, command: &str) -> ExecuteCommandRequest {
        ExecuteCommandRequest {
            session_key: session_key.into(),
            command: command.into(),
            custom_cwd: None,
            new_terminal: false,
            background: false,
            timeout_millis: 5_000,
            terminal_id: None,
            env: Vec::new(),
        }
    }

    fn environment_variable(key: &str, value: &str) -> chelix_protocol::ToolsServiceEnvVar {
        chelix_protocol::ToolsServiceEnvVar {
            key: key.into(),
            value: value.into(),
            secret: false,
        }
    }

    async fn wait_until_idle(manager: &TerminalManager, session_key: &str, terminal_id: &str) {
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let info = manager
                    .terminal_info(session_key, terminal_id)
                    .await
                    .unwrap_or_else(|error| panic!("terminal info failed: {error}"));
                if !info.running {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        assert!(result.is_ok(), "terminal {terminal_id} did not become idle");
    }

    #[test]
    fn shell_event_parser_removes_completion_control_sequences() {
        let mut parser = ShellEventParser::default();
        let parsed = parser.feed(b"hello\n\x1b]633;C\x07body\x1b]633;D;7\x07prompt");
        assert_eq!(parsed.items, vec![
            ParsedOutputItem::Output(b"hello\n".to_vec()),
            ParsedOutputItem::CommandStarted,
            ParsedOutputItem::Output(b"body".to_vec()),
            ParsedOutputItem::CommandFinished(7),
            ParsedOutputItem::Output(b"prompt".to_vec()),
        ]);
    }

    #[test]
    fn shell_event_parser_handles_split_sequences() {
        let mut parser = ShellEventParser::default();
        let first = parser.feed(b"output\x1b]63");
        assert_eq!(first.items, vec![ParsedOutputItem::Output(
            b"output".to_vec()
        )]);
        let second = parser.feed(b"3;D;0\x07");
        assert_eq!(second.items, vec![ParsedOutputItem::CommandFinished(0)]);
    }

    #[test]
    fn shell_event_parser_preserves_output_around_split_sequences() {
        let mut parser = ShellEventParser::default();
        let first = parser.feed(b"before\x1b]63");
        let second = parser.feed(b"3;C\x07middle\x1b]633;D;4\x07after");

        assert_eq!(first.items, vec![ParsedOutputItem::Output(
            b"before".to_vec()
        )]);
        assert_eq!(second.items, vec![
            ParsedOutputItem::CommandStarted,
            ParsedOutputItem::Output(b"middle".to_vec()),
            ParsedOutputItem::CommandFinished(4),
            ParsedOutputItem::Output(b"after".to_vec()),
        ]);
    }

    #[test]
    fn shell_event_parser_flushes_incomplete_sequence_as_output() {
        let mut parser = ShellEventParser::default();
        let parsed = parser.feed(b"visible\x1b]633;D;");

        assert_eq!(parsed.items, vec![ParsedOutputItem::Output(
            b"visible".to_vec()
        )]);
        assert_eq!(parser.finish(), b"\x1b]633;D;");
    }

    #[test]
    fn line_limit_returns_most_recent_terminal_lines() {
        let text = "one\ntwo\nthree\nfour".to_string();

        assert_eq!(limit_lines(text.clone(), None), text);
        assert_eq!(limit_lines(text.clone(), Some(2)), "three\nfour");
        assert_eq!(limit_lines(text, Some(0)), "");
    }

    #[test]
    fn retained_text_joins_wrapped_screen_rows() {
        let mut screen = TerminalScreen::new(TerminalSize::new(5, 2), usize::MAX);
        screen.feed(b"abcdefgh");

        let text = retained_text(screen.screen(), None)
            .unwrap_or_else(|error| panic!("retained text failed: {error}"));

        assert_eq!(text, "abcdefgh");
    }

    #[test]
    fn environment_names_are_strictly_validated() {
        assert!(validate_env_key("TOKEN_1").is_ok());
        assert!(validate_env_key("1TOKEN").is_err());
        assert!(validate_env_key("BAD-NAME").is_err());
    }

    #[test]
    fn environment_fingerprint_is_deterministic_and_order_independent() {
        let first = validated_environment(&[
            environment_variable("ALPHA", "one"),
            environment_variable("BETA", "two"),
        ])
        .unwrap_or_else(|error| panic!("first environment validation failed: {error}"));
        let reordered = validated_environment(&[
            environment_variable("BETA", "two"),
            environment_variable("ALPHA", "one"),
        ])
        .unwrap_or_else(|error| panic!("reordered environment validation failed: {error}"));

        assert_eq!(
            environment_fingerprint(&first),
            environment_fingerprint(&first)
        );
        assert_eq!(
            environment_fingerprint(&first),
            environment_fingerprint(&reordered)
        );
    }

    #[test]
    fn environment_fingerprint_changes_with_key_or_value() {
        let original = validated_environment(&[environment_variable("TOKEN", "one")])
            .unwrap_or_else(|error| panic!("original environment validation failed: {error}"));
        let changed_key = validated_environment(&[environment_variable("OTHER_TOKEN", "one")])
            .unwrap_or_else(|error| panic!("changed-key validation failed: {error}"));
        let changed_value = validated_environment(&[environment_variable("TOKEN", "two")])
            .unwrap_or_else(|error| panic!("changed-value validation failed: {error}"));

        assert_ne!(
            environment_fingerprint(&original),
            environment_fingerprint(&changed_key)
        );
        assert_ne!(
            environment_fingerprint(&original),
            environment_fingerprint(&changed_value)
        );
    }

    #[test]
    fn duplicate_environment_keys_are_rejected() {
        let error = match validated_environment(&[
            environment_variable("TOKEN", "one"),
            environment_variable("TOKEN", "two"),
        ]) {
            Ok(_) => panic!("expected duplicate environment key error"),
            Err(error) => error,
        };

        assert_eq!(error.to_string(), "duplicate environment variable: TOKEN");
    }

    #[test]
    fn shell_quoting_preserves_single_quotes() {
        assert_eq!(shell_quote("a'b").unwrap_or_default(), "'a'\\''b'");
    }

    #[test]
    fn terminal_inventory_is_ordered_by_numeric_id() {
        let infos = ["50", "47", "48"]
            .into_iter()
            .map(|id| ToolsServiceTerminalInfo {
                id: id.into(),
                session_key: "session:tabs".into(),
                running: false,
                alive: true,
            })
            .collect();

        let ordered = order_terminal_infos(infos)
            .unwrap_or_else(|error| panic!("terminal inventory ordering failed: {error}"));
        assert_eq!(
            ordered
                .iter()
                .map(|terminal| terminal.id.as_str())
                .collect::<Vec<_>>(),
            vec!["47", "48", "50"]
        );
    }

    #[tokio::test]
    async fn interactive_terminals_use_numeric_ids_and_enforce_session_ownership() {
        let manager = TerminalManager::new(std::env::temp_dir())
            .unwrap_or_else(|error| panic!("terminal manager setup failed: {error}"));

        let first = manager
            .create_interactive_terminal("session:first", &[])
            .await
            .unwrap_or_else(|error| panic!("first terminal creation failed: {error}"));
        let second = manager
            .create_interactive_terminal("session:second", &[])
            .await
            .unwrap_or_else(|error| panic!("second terminal creation failed: {error}"));

        assert_eq!(first.id, "1");
        assert_eq!(second.id, "2");
        assert!(first.id.parse::<u64>().is_ok());
        assert!(second.id.parse::<u64>().is_ok());
        assert_eq!(
            manager
                .terminal_ids("session:first")
                .await
                .unwrap_or_else(|error| panic!("terminal listing failed: {error}")),
            vec!["1"]
        );
        let ownership_error = match manager.terminal_info("session:first", "2").await {
            Ok(_) => panic!("expected terminal ownership error"),
            Err(error) => error,
        };
        assert_eq!(
            ownership_error.to_string(),
            "terminal 2 does not belong to this session"
        );

        manager
            .shutdown()
            .await
            .unwrap_or_else(|error| panic!("terminal shutdown failed: {error}"));
    }

    #[tokio::test]
    async fn background_command_transitions_from_running_to_idle() {
        let manager = TerminalManager::new(std::env::temp_dir())
            .unwrap_or_else(|error| panic!("terminal manager setup failed: {error}"));
        let mut request = command_request("session:background", "sleep 1");
        request.new_terminal = true;
        request.background = true;

        let response = manager
            .execute_command(request)
            .await
            .unwrap_or_else(|error| panic!("background command failed: {error}"));
        assert!(response.background);
        assert!(!response.completed);
        assert!(
            manager
                .terminal_info("session:background", &response.terminal_id)
                .await
                .unwrap_or_else(|error| panic!("terminal info failed: {error}"))
                .running
        );

        wait_until_idle(&manager, "session:background", &response.terminal_id).await;
        let output = manager
            .read_terminal_output(ReadTerminalOutputRequest {
                session_key: "session:background".into(),
                terminal_id: response.terminal_id,
                max_lines: None,
            })
            .await
            .unwrap_or_else(|error| panic!("terminal output read failed: {error}"));
        assert!(output.completed);
        assert!(!output.running);
        assert!(output.alive);

        manager
            .shutdown()
            .await
            .unwrap_or_else(|error| panic!("terminal shutdown failed: {error}"));
    }

    #[tokio::test]
    async fn commands_reuse_persistent_shell_state() {
        let manager = TerminalManager::new(std::env::temp_dir())
            .unwrap_or_else(|error| panic!("terminal manager setup failed: {error}"));
        let mut setup = command_request(
            "session:state",
            "export CHELIX_TEST_STATE=persisted; cd /tmp; chelix_test_fn() { printf 'function-state'; }",
        );
        setup.new_terminal = true;

        let setup_response = manager
            .execute_command(setup)
            .await
            .unwrap_or_else(|error| panic!("shell state setup failed: {error}"));
        assert!(setup_response.completed);

        let mut verify = command_request(
            "session:state",
            "printf '%s|%s|' \"$CHELIX_TEST_STATE\" \"$PWD\"; chelix_test_fn; printf '\\n'",
        );
        verify.terminal_id = Some(setup_response.terminal_id.clone());
        let verify_response = manager
            .execute_command(verify)
            .await
            .unwrap_or_else(|error| panic!("shell state verification failed: {error}"));

        assert_eq!(verify_response.terminal_id, setup_response.terminal_id);
        assert_eq!(
            verify_response.output.trim(),
            "persisted|/tmp|function-state"
        );

        manager
            .shutdown()
            .await
            .unwrap_or_else(|error| panic!("terminal shutdown failed: {error}"));
    }

    #[tokio::test]
    async fn terminal_rejects_reuse_with_changed_environment() {
        let manager = TerminalManager::new(std::env::temp_dir())
            .unwrap_or_else(|error| panic!("terminal manager setup failed: {error}"));
        let mut initial = command_request("session:environment", "printf 'ready\\n'");
        initial.new_terminal = true;
        initial.env = vec![environment_variable("CHELIX_TEST_ENV", "initial")];

        let response = manager
            .execute_command(initial)
            .await
            .unwrap_or_else(|error| panic!("initial command failed: {error}"));
        let mut changed = command_request("session:environment", "printf 'unexpected\\n'");
        changed.terminal_id = Some(response.terminal_id.clone());
        changed.env = vec![environment_variable("CHELIX_TEST_ENV", "changed")];

        let error = match manager.execute_command(changed).await {
            Ok(_) => panic!("expected changed environment rejection"),
            Err(error) => error,
        };
        assert_eq!(
            error.to_string(),
            format!(
                "terminal {} was created with a different environment; create a new terminal to apply changed variables",
                response.terminal_id
            )
        );

        manager
            .shutdown()
            .await
            .unwrap_or_else(|error| panic!("terminal shutdown failed: {error}"));
    }

    #[tokio::test]
    async fn terminal_subscription_returns_new_append_only_pty_bytes() {
        let manager = TerminalManager::new(std::env::temp_dir())
            .unwrap_or_else(|error| panic!("terminal manager setup failed: {error}"));
        let mut initial = command_request("session:subscription", "printf 'ready\\n'");
        initial.new_terminal = true;
        let response = manager
            .execute_command(initial)
            .await
            .unwrap_or_else(|error| panic!("initial command failed: {error}"));
        let mut subscription = manager
            .subscribe_terminal("session:subscription", &response.terminal_id)
            .await
            .unwrap_or_else(|error| panic!("terminal subscription failed: {error}"));
        assert!(!subscription.initial_output.is_empty());

        let mut colored = command_request(
            "session:subscription",
            "printf '\\033[31mraw-output\\033[0m\\n'",
        );
        colored.terminal_id = Some(response.terminal_id.clone());
        manager
            .execute_command(colored)
            .await
            .unwrap_or_else(|error| panic!("colored command failed: {error}"));

        let marker = b"\x1b[31mraw-output\x1b[0m";
        let appended = tokio::time::timeout(Duration::from_secs(5), async {
            let mut appended = Vec::new();
            loop {
                match subscription.next_output().await {
                    Ok(Some(bytes)) => appended.extend_from_slice(&bytes),
                    Ok(None) => panic!("terminal closed before subscribed output arrived"),
                    Err(error) => panic!("subscribed output read failed: {error}"),
                }
                if appended
                    .windows(marker.len())
                    .any(|window| window == marker)
                {
                    return appended;
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("subscribed output did not arrive"));

        assert!(
            appended
                .windows(marker.len())
                .any(|window| window == marker)
        );
        assert!(!appended.windows(5).any(|window| window == b"ready"));

        manager
            .shutdown()
            .await
            .unwrap_or_else(|error| panic!("terminal shutdown failed: {error}"));
    }
}
