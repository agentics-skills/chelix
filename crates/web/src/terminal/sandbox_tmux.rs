use std::collections::HashMap;

use portable_pty::CommandBuilder;

use {moltis_httpd::AppState, tokio::process::Command};

use super::types::{
    SandboxTerminalTarget, SandboxTmuxPaneInfo, SandboxTmuxSessionInfo, SandboxTmuxTree,
    SandboxTmuxWindowInfo, TerminalResult,
};

const SANDBOX_WORKDIR: &str = "/home/sandbox";
const APPLE_CONTAINER_WORKDIR: &str = "/tmp";
const FIELD_SEP: &str = "|moltis-tmux-field|";

#[derive(Debug)]
struct SandboxCommandOutput {
    stdout: String,
    stderr: String,
    exit_code: i32,
}

#[derive(Debug, Clone)]
struct RawWindowInfo {
    session_id: String,
    id: String,
    index: u32,
    name: String,
    active: bool,
}

#[derive(Debug, Clone)]
struct RawPaneInfo {
    window_id: String,
    id: String,
    index: u32,
    active: bool,
    current_command: String,
    current_path: String,
    title: String,
}

pub(crate) async fn sandbox_terminal_targets(
    state: &AppState,
) -> TerminalResult<Vec<SandboxTerminalTarget>> {
    let prefix = state
        .gateway
        .sandbox_router
        .as_ref()
        .map(|router| {
            router
                .config()
                .container_prefix
                .clone()
                .unwrap_or_else(|| "moltis-sandbox".to_string())
        })
        .unwrap_or_else(|| "moltis-sandbox".to_string());
    let containers = moltis_tools::sandbox::list_running_containers(&prefix)
        .await
        .map_err(|err| format!("failed to list sandbox terminal targets: {err}"))?;
    let mut targets: Vec<SandboxTerminalTarget> = containers
        .into_iter()
        .filter(|container| {
            matches!(
                container.state,
                moltis_tools::sandbox::ContainerRunState::Running
            )
        })
        .map(|container| {
            let backend = container_backend_name(container.backend).to_string();
            let state = container_state_name(container.state).to_string();
            let id = format!("{backend}:{}", container.name);
            let label = format!("{} ({backend})", container.name);
            SandboxTerminalTarget {
                id,
                label,
                backend,
                container_name: container.name,
                state,
                image: container.image,
            }
        })
        .collect();
    targets.sort_by(|a, b| a.label.cmp(&b.label));
    Ok(targets)
}

pub(crate) async fn resolve_sandbox_terminal_target(
    state: &AppState,
    target_id: &str,
) -> TerminalResult<SandboxTerminalTarget> {
    let requested = target_id.trim();
    if requested.is_empty() {
        return Err("sandbox terminal target id is required".into());
    }
    sandbox_terminal_targets(state)
        .await?
        .into_iter()
        .find(|target| target.id == requested)
        .ok_or_else(|| format!("sandbox terminal target not found: {requested}").into())
}

pub(crate) async fn sandbox_tmux_tree(
    target: &SandboxTerminalTarget,
) -> TerminalResult<SandboxTmuxTree> {
    let session_format = tmux_format(&["session_id", "session_name", "session_attached"]);
    let session_output =
        run_sandbox_tmux(target, &["list-sessions", "-F", &session_format]).await?;
    if is_tmux_no_server(&session_output) {
        return Ok(SandboxTmuxTree {
            available: true,
            reason: None,
            sessions: Vec::new(),
        });
    }
    if session_output.exit_code != 0 {
        if is_tmux_missing(&session_output) {
            return Ok(SandboxTmuxTree {
                available: false,
                reason: Some("tmux_not_installed".to_string()),
                sessions: Vec::new(),
            });
        }
        return Err(format!(
            "failed to list sandbox tmux sessions: {}",
            command_error_text(&session_output)
        )
        .into());
    }

    let mut sessions = parse_sessions(&session_output.stdout)?;
    if sessions.is_empty() {
        return Ok(SandboxTmuxTree {
            available: true,
            reason: None,
            sessions,
        });
    }

    let window_format = tmux_format(&[
        "session_id",
        "window_id",
        "window_index",
        "window_name",
        "window_active",
    ]);
    let pane_format = tmux_format(&[
        "session_id",
        "window_id",
        "pane_id",
        "pane_index",
        "pane_active",
        "pane_current_command",
        "pane_current_path",
        "pane_title",
    ]);
    let windows = run_sandbox_tmux(target, &["list-windows", "-aF", &window_format]).await?;
    if windows.exit_code != 0 && !is_tmux_no_server(&windows) {
        return Err(format!(
            "failed to list sandbox tmux windows: {}",
            command_error_text(&windows)
        )
        .into());
    }
    let panes = run_sandbox_tmux(target, &["list-panes", "-aF", &pane_format]).await?;
    if panes.exit_code != 0 && !is_tmux_no_server(&panes) {
        return Err(format!(
            "failed to list sandbox tmux panes: {}",
            command_error_text(&panes)
        )
        .into());
    }

    let mut windows_by_session: HashMap<String, Vec<RawWindowInfo>> = HashMap::new();
    for window in parse_windows(&windows.stdout)? {
        windows_by_session
            .entry(window.session_id.clone())
            .or_default()
            .push(window);
    }
    let mut panes_by_window: HashMap<String, Vec<RawPaneInfo>> = HashMap::new();
    for pane in parse_panes(&panes.stdout)? {
        panes_by_window
            .entry(pane.window_id.clone())
            .or_default()
            .push(pane);
    }

    for session in &mut sessions {
        let mut raw_windows = windows_by_session.remove(&session.id).unwrap_or_default();
        raw_windows.sort_by_key(|window| window.index);
        session.windows = raw_windows
            .into_iter()
            .map(|window| {
                let mut raw_panes = panes_by_window.remove(&window.id).unwrap_or_default();
                raw_panes.sort_by_key(|pane| pane.index);
                SandboxTmuxWindowInfo {
                    id: window.id,
                    index: window.index,
                    name: window.name,
                    active: window.active,
                    panes: raw_panes
                        .into_iter()
                        .map(|pane| SandboxTmuxPaneInfo {
                            id: pane.id,
                            index: pane.index,
                            active: pane.active,
                            current_command: pane.current_command,
                            current_path: pane.current_path,
                            title: pane.title,
                        })
                        .collect(),
                }
            })
            .collect();
    }

    Ok(SandboxTmuxTree {
        available: true,
        reason: None,
        sessions,
    })
}

pub(crate) async fn sandbox_tmux_prepare_attach(
    target: &SandboxTerminalTarget,
    session_id: &str,
    window_id: Option<&str>,
    pane_id: Option<&str>,
) -> TerminalResult<()> {
    let tree = sandbox_tmux_tree(target).await?;
    if !tree.available {
        return Err(tree
            .reason
            .unwrap_or_else(|| "sandbox tmux is unavailable".to_string())
            .into());
    }
    let Some(session) = tree
        .sessions
        .iter()
        .find(|session| session.id == session_id)
    else {
        return Err(format!("sandbox tmux session not found: {session_id}").into());
    };

    if let Some(window_id) = window_id {
        let Some(window) = session.windows.iter().find(|window| window.id == window_id) else {
            return Err(format!("sandbox tmux window not found: {window_id}").into());
        };
        let output = run_sandbox_tmux(target, &["select-window", "-t", &window.id]).await?;
        if output.exit_code != 0 {
            return Err(format!(
                "failed to select sandbox tmux window: {}",
                command_error_text(&output)
            )
            .into());
        }
    }

    if let Some(pane_id) = pane_id {
        let pane_exists = session
            .windows
            .iter()
            .flat_map(|window| window.panes.iter())
            .any(|pane| pane.id == pane_id);
        if !pane_exists {
            return Err(format!("sandbox tmux pane not found: {pane_id}").into());
        }
        let output = run_sandbox_tmux(target, &["select-pane", "-t", pane_id]).await?;
        if output.exit_code != 0 {
            return Err(format!(
                "failed to select sandbox tmux pane: {}",
                command_error_text(&output)
            )
            .into());
        }
    }

    Ok(())
}

pub(crate) fn sandbox_terminal_tmux_attach_command_builder(
    target: &SandboxTerminalTarget,
    session_id: &str,
) -> CommandBuilder {
    let cli = backend_cli(&target.backend);
    let mut cmd = CommandBuilder::new(cli);
    if target.backend == "apple-container" {
        cmd.args([
            "exec",
            "--workdir",
            APPLE_CONTAINER_WORKDIR,
            &target.container_name,
            "tmux",
            "attach-session",
            "-t",
            session_id,
        ]);
        return cmd;
    }
    cmd.args([
        "exec",
        "-it",
        "-w",
        SANDBOX_WORKDIR,
        &target.container_name,
        "tmux",
        "attach-session",
        "-t",
        session_id,
    ]);
    cmd
}

async fn run_sandbox_tmux(
    target: &SandboxTerminalTarget,
    tmux_args: &[&str],
) -> TerminalResult<SandboxCommandOutput> {
    let cli = backend_cli(&target.backend);
    let output = if target.backend == "apple-container" {
        let mut command = Command::new(cli);
        command.args([
            "exec",
            "--workdir",
            APPLE_CONTAINER_WORKDIR,
            &target.container_name,
            "tmux",
        ]);
        command.args(tmux_args);
        command.output().await
    } else {
        let mut command = Command::new(cli);
        command.args([
            "exec",
            "-w",
            SANDBOX_WORKDIR,
            &target.container_name,
            "tmux",
        ]);
        command.args(tmux_args);
        command.output().await
    }
    .map_err(|err| format!("failed to execute sandbox tmux command: {err}"))?;

    Ok(SandboxCommandOutput {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        exit_code: output.status.code().unwrap_or(-1),
    })
}

fn backend_cli(backend: &str) -> &str {
    match backend {
        "apple-container" => "container",
        "podman" => "podman",
        _ => "docker",
    }
}

fn container_backend_name(backend: moltis_tools::sandbox::ContainerBackend) -> &'static str {
    match backend {
        moltis_tools::sandbox::ContainerBackend::AppleContainer => "apple-container",
        moltis_tools::sandbox::ContainerBackend::Docker => "docker",
        moltis_tools::sandbox::ContainerBackend::Podman => "podman",
    }
}

fn container_state_name(state: moltis_tools::sandbox::ContainerRunState) -> &'static str {
    match state {
        moltis_tools::sandbox::ContainerRunState::Running => "running",
        moltis_tools::sandbox::ContainerRunState::Stopped => "stopped",
        moltis_tools::sandbox::ContainerRunState::Exited => "exited",
        moltis_tools::sandbox::ContainerRunState::Unknown => "unknown",
    }
}

fn tmux_format(fields: &[&str]) -> String {
    fields
        .iter()
        .map(|field| format!("#{{{field}}}"))
        .collect::<Vec<_>>()
        .join(FIELD_SEP)
}

fn is_tmux_no_server(output: &SandboxCommandOutput) -> bool {
    output.exit_code != 0
        && command_error_text(output)
            .to_ascii_lowercase()
            .contains("no server running")
}

fn is_tmux_missing(output: &SandboxCommandOutput) -> bool {
    let text = command_error_text(output).to_ascii_lowercase();
    text.contains("tmux: command not found")
        || text.contains("executable file not found")
        || text.contains("no such file or directory")
        || text.contains("command not found")
}

fn command_error_text(output: &SandboxCommandOutput) -> String {
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

fn parse_bool_flag(raw: &str) -> bool {
    raw.trim() == "1"
}

fn parse_sessions(stdout: &str) -> TerminalResult<Vec<SandboxTmuxSessionInfo>> {
    let mut sessions = Vec::new();
    for line in stdout.lines().filter(|line| !line.trim().is_empty()) {
        let parts: Vec<&str> = line.split(FIELD_SEP).collect();
        if parts.len() != 3 {
            return Err(format!("invalid tmux session line: {line}").into());
        }
        sessions.push(SandboxTmuxSessionInfo {
            id: parts[0].to_string(),
            name: parts[1].to_string(),
            attached: parse_bool_flag(parts[2]),
            windows: Vec::new(),
        });
    }
    Ok(sessions)
}

fn parse_windows(stdout: &str) -> TerminalResult<Vec<RawWindowInfo>> {
    let mut windows = Vec::new();
    for line in stdout.lines().filter(|line| !line.trim().is_empty()) {
        let parts: Vec<&str> = line.split(FIELD_SEP).collect();
        if parts.len() != 5 {
            return Err(format!("invalid tmux window line: {line}").into());
        }
        let index = parts[2]
            .parse::<u32>()
            .map_err(|err| format!("invalid tmux window index '{}': {err}", parts[2]))?;
        windows.push(RawWindowInfo {
            session_id: parts[0].to_string(),
            id: parts[1].to_string(),
            index,
            name: parts[3].to_string(),
            active: parse_bool_flag(parts[4]),
        });
    }
    Ok(windows)
}

fn parse_panes(stdout: &str) -> TerminalResult<Vec<RawPaneInfo>> {
    let mut panes = Vec::new();
    for line in stdout.lines().filter(|line| !line.trim().is_empty()) {
        let parts: Vec<&str> = line.split(FIELD_SEP).collect();
        if parts.len() != 8 {
            return Err(format!("invalid tmux pane line: {line}").into());
        }
        let index = parts[3]
            .parse::<u32>()
            .map_err(|err| format!("invalid tmux pane index '{}': {err}", parts[3]))?;
        panes.push(RawPaneInfo {
            window_id: parts[1].to_string(),
            id: parts[2].to_string(),
            index,
            active: parse_bool_flag(parts[4]),
            current_command: parts[5].to_string(),
            current_path: parts[6].to_string(),
            title: parts[7].to_string(),
        });
    }
    Ok(panes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_sessions_reads_ids_names_and_attached_flag() {
        let sep = FIELD_SEP;
        let input = format!("$0{sep}main{sep}1\n$1{sep}proc-a{sep}0\n");
        let sessions = parse_sessions(&input).expect("sessions should parse");
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].id, "$0");
        assert_eq!(sessions[0].name, "main");
        assert!(sessions[0].attached);
        assert_eq!(sessions[1].id, "$1");
        assert!(!sessions[1].attached);
    }

    #[test]
    fn parse_sessions_accepts_live_tmux_separator_output() {
        let input = "$0|moltis-tmux-field|ivan-bash-2|moltis-tmux-field|0\n";
        let sessions = parse_sessions(input).expect("live tmux session output should parse");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "$0");
        assert_eq!(sessions[0].name, "ivan-bash-2");
        assert!(!sessions[0].attached);
    }

    #[test]
    fn parse_windows_reads_session_window_metadata() {
        let sep = FIELD_SEP;
        let input = format!("$0{sep}@1{sep}0{sep}bash{sep}1\n");
        let windows = parse_windows(&input).expect("windows should parse");
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].session_id, "$0");
        assert_eq!(windows[0].id, "@1");
        assert_eq!(windows[0].index, 0);
        assert_eq!(windows[0].name, "bash");
        assert!(windows[0].active);
    }

    #[test]
    fn parse_panes_reads_command_path_and_title() {
        let sep = FIELD_SEP;
        let input = format!("$0{sep}@1{sep}%2{sep}0{sep}1{sep}bash{sep}/home/sandbox{sep}agent\n");
        let panes = parse_panes(&input).expect("panes should parse");
        assert_eq!(panes.len(), 1);
        assert_eq!(panes[0].window_id, "@1");
        assert_eq!(panes[0].id, "%2");
        assert_eq!(panes[0].current_command, "bash");
        assert_eq!(panes[0].current_path, "/home/sandbox");
        assert_eq!(panes[0].title, "agent");
    }
}
