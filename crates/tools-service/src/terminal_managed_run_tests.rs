use super::*;

const SESSION: &str = "session:managed-run";

fn request(command: &str, terminal_id: Option<&str>) -> ExecuteCommandRequest {
    ExecuteCommandRequest {
        session_key: SESSION.into(),
        tool_call_id: Uuid::new_v4().to_string(),
        command: command.into(),
        custom_cwd: None,
        new_terminal: terminal_id.is_none(),
        background: false,
        timeout_millis: 5_000,
        terminal_id: terminal_id.map(str::to_owned),
        env: Vec::new(),
    }
}

async fn setup() -> (Arc<TerminalManager>, Arc<ManagedTerminal>) {
    let manager = Arc::new(
        TerminalManager::new(std::env::temp_dir())
            .unwrap_or_else(|error| panic!("manager setup failed: {error}")),
    );
    let response = manager
        .execute_command(request("PS1='$ '; PS2='> '", None))
        .await
        .unwrap_or_else(|error| panic!("shell setup failed: {error}"));
    assert!(response.completed);
    let terminal = manager
        .find_terminal(SESSION, &response.terminal_id)
        .await
        .unwrap_or_else(|error| panic!("terminal lookup failed: {error}"));
    (manager, terminal)
}

async fn execute(
    manager: &TerminalManager,
    terminal: &ManagedTerminal,
    command: &str,
) -> ExecuteCommandResponse {
    manager
        .execute_command(request(command, Some(&terminal.id)))
        .await
        .unwrap_or_else(|error| panic!("command execution failed: {error}"))
}

async fn read(manager: &TerminalManager, terminal: &ManagedTerminal) -> ReadTerminalOutputResponse {
    manager
        .read_terminal_output(ReadTerminalOutputRequest {
            session_key: SESSION.into(),
            terminal_id: terminal.id.clone(),
            max_lines: None,
        })
        .await
        .unwrap_or_else(|error| panic!("terminal read failed: {error}"))
}

async fn wait_for_state(terminal: &ManagedTerminal, predicate: impl Fn(&TerminalOutput) -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let notified = terminal.output_notify.notified();
            if predicate(&lock(&terminal.output)) {
                return;
            }
            notified.await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        let output = lock(&terminal.output);
        panic!(
            "terminal state timed out: {:?}; history: {:?}",
            output.run_error,
            String::from_utf8_lossy(&output.history)
        );
    });
}

async fn shutdown(manager: &TerminalManager) {
    manager
        .shutdown()
        .await
        .unwrap_or_else(|error| panic!("shutdown failed: {error}"));
}

fn state() -> TerminalOutput {
    TerminalOutput {
        history: Vec::new(),
        screen: TerminalScreen::new(TerminalSize::new(80, 24), usize::MAX),
        parser: ShellEventParser::default(),
        active_run: None,
        last_exit_code: None,
        ready: false,
        at_prompt: false,
        input_prompt: None,
        prompt_id: None,
        pending_prompt_id: None,
        pending_prompt_kind: None,
        primary_prompt_pending: false,
        run_error: None,
        closed: false,
    }
}

fn feed(output: &mut TerminalOutput, bytes: &[u8]) {
    for item in output.parser.feed(bytes).items {
        process_output_item(output, item)
            .unwrap_or_else(|error| panic!("output processing failed: {error}"));
    }
}

fn start_run(output: &mut TerminalOutput) {
    output.active_run = Some(ManagedRun {
        id: "run".into(),
        submission_line: output.screen.screen().cursor_absolute_y(),
        output_start_line: None,
        command_output_end_line: None,
        output_end_line: None,
        command_started_count: 0,
        command_finished_count: 0,
        input_complete: false,
        exit_code: None,
        completed: false,
    });
}

fn captured(output: &TerminalOutput) -> String {
    let run = output
        .active_run
        .as_ref()
        .unwrap_or_else(|| panic!("missing run"));
    run_output(output, run).unwrap_or_else(|error| panic!("capture failed: {error}"))
}

fn submitted(output: &mut TerminalOutput, last: bool) {
    assert!(output.input_prompt.take().is_some());
    output.at_prompt = false;
    output
        .active_run
        .as_mut()
        .unwrap_or_else(|| panic!("missing run"))
        .input_complete = last;
}

#[test]
fn prompt_id_expression_matches_both_shell_prompt_wrappers() {
    let property = format!("P;ChelixPromptId={PROMPT_ID_EXPRESSION}");
    assert_eq!(PROMPT_COMMAND.matches(&property).count(), 2);
    let mut parser = ShellEventParser::default();
    let sequence = format!("\x1b]633;{property}\x07");
    assert_eq!(parser.feed(sequence.as_bytes()).items, vec![
        ParsedOutputItem::PromptExpansionDisabled
    ]);
}

#[test]
fn prompt_parser_handles_every_chunk_boundary() {
    let bytes = b"\x1b]633;P;ChelixPromptId=12\x07\x1b]633;A\x07\x1b]633;B\x07\x1b]633;C\x07\x1b]633;D;7\x07\x1b]633;P;ChelixPromptId=13\x07\x1b]633;F\x07\x1b]633;G\x07";
    let expected = vec![
        ParsedOutputItem::PromptId(12),
        ParsedOutputItem::PromptStarted(PromptKind::Primary),
        ParsedOutputItem::PromptFinished(PromptKind::Primary),
        ParsedOutputItem::CommandStarted,
        ParsedOutputItem::CommandFinished(7),
        ParsedOutputItem::PromptId(13),
        ParsedOutputItem::PromptStarted(PromptKind::Continuation),
        ParsedOutputItem::PromptFinished(PromptKind::Continuation),
    ];
    for split in 0..=bytes.len() {
        let mut parser = ShellEventParser::default();
        let mut items = parser.feed(&bytes[..split]).items;
        items.extend(parser.feed(&bytes[split..]).items);
        assert_eq!(items, expected, "split at {split}");
        assert!(parser.finish().is_empty());
    }
    let mut parser = ShellEventParser::default();
    assert_eq!(
        parser
            .feed(b"\x1b]633;P;ChelixPromptId=$((counter))\x07")
            .items,
        vec![ParsedOutputItem::InvalidMarker(
            "invalid RMUX shell prompt identifier"
        )]
    );
}

#[test]
fn invalid_markers_preserve_prompt_state_without_acknowledging_input() {
    let mut output = state();
    start_run(&mut output);
    feed(
        &mut output,
        b"\x1b]633;D;invalid\x07\x1b]633;P;ChelixPromptId=invalid\x07",
    );
    assert_eq!(output.input_prompt, None);
    assert!(!output.primary_prompt_pending);
    assert!(output.run_error.is_none());
    assert!(
        !output
            .active_run
            .as_ref()
            .unwrap_or_else(|| panic!("missing run"))
            .completed
    );
    feed(
        &mut output,
        b"\x1b]633;D;0\x07\x1b]633;P;ChelixPromptId=1\x07\x1b]633;A\x07",
    );
    feed(&mut output, b"\x1b]633;P;ChelixPromptId=invalid\x07");
    assert_eq!(output.pending_prompt_id, Some(1));
    assert_eq!(output.pending_prompt_kind, Some(PromptKind::Primary));
    assert!(output.primary_prompt_pending);
    feed(&mut output, b"\x1b]633;B\x07");
    assert_eq!(output.input_prompt, Some(PromptKind::Primary));
    submitted(&mut output, true);
    feed(
        &mut output,
        b"\x1b]633;P;ChelixPromptId=2\x07\x1b]633;F\x07\x1b]633;D;invalid\x07\x1b]633;G\x07",
    );
    assert_eq!(output.input_prompt, Some(PromptKind::Continuation));
    assert!(terminal_running(&output));
    assert!(output.run_error.is_none());
}

#[test]
fn run_counts_boundaries_and_completes_only_after_the_last_primary_prompt() {
    let mut output = state();
    feed(
        &mut output,
        b"\x1b]633;D;0\x07\x1b]633;P;ChelixPromptId=1\x07\x1b]633;A\x07$ \x1b]633;B\x07",
    );
    assert!(output.ready);
    assert_eq!(output.last_exit_code, None);
    start_run(&mut output);
    assert_eq!(captured(&output), "");
    submitted(&mut output, false);
    feed(&mut output, b"echo one\r\n");
    assert_eq!(captured(&output), "");
    feed(&mut output, b"\x1b]633;C\x07one");
    assert_eq!(captured(&output), "one");
    feed(&mut output, b"\r\n\x1b]633;D;0\x07");
    assert!(terminal_running(&output));
    assert_eq!(output.input_prompt, None);
    assert_eq!(captured(&output), "one");
    feed(
        &mut output,
        b"\x1b]633;P;ChelixPromptId=2\x07\x1b]633;A\x07$ \x1b]633;B\x07",
    );
    assert!(terminal_running(&output));
    assert_eq!(captured(&output), "one");
    submitted(&mut output, true);
    feed(&mut output, b"echo two; false\r\n\x1b]633;C\x07two");
    assert_eq!(captured(&output), "$ echo one\none\n$ echo two; false\ntwo");
    feed(&mut output, b"\r\n\x1b]633;D;1\x07");
    assert!(terminal_running(&output));
    feed(
        &mut output,
        b"\x1b]633;P;ChelixPromptId=3\x07\x1b]633;A\x07$ \x1b]633;B\x07",
    );
    assert!(!terminal_running(&output));
    let run = output
        .active_run
        .as_ref()
        .unwrap_or_else(|| panic!("missing run"));
    assert_eq!(run.command_started_count, 2);
    assert_eq!(run.command_finished_count, 2);
    assert_eq!(run.exit_code, Some(1));
    assert!(run.completed);
    assert_eq!(
        captured(&output),
        "$ echo one\none\n$ echo two; false\ntwo\n$"
    );
    let end = run.output_end_line;
    feed(&mut output, b"true\r\n\x1b]633;C\x07\x1b]633;D;0\x07\x1b]633;P;ChelixPromptId=4\x07\x1b]633;A\x07$ \x1b]633;B\x07");
    let run = output
        .active_run
        .as_ref()
        .unwrap_or_else(|| panic!("missing run"));
    assert_eq!(run.command_finished_count, 2);
    assert_eq!(run.exit_code, Some(1));
    assert_eq!(run.output_end_line, end);
    assert_eq!(output.last_exit_code, Some(0));
}

#[test]
fn cached_prompt_redraws_do_not_acknowledge_input_and_primary_accepts_counter_reset() {
    let mut output = state();
    feed(
        &mut output,
        b"\x1b]633;D;0\x07\x1b]633;P;ChelixPromptId=1\x07\x1b]633;A\x07$ \x1b]633;B\x07",
    );
    start_run(&mut output);
    submitted(&mut output, false);
    feed(
        &mut output,
        b"\x1b]633;P;ChelixPromptId=1\x07\x1b]633;A\x07$ \x1b]633;B\x07",
    );
    assert_eq!(output.input_prompt, None);
    feed(
        &mut output,
        b"\x1b]633;P;ChelixPromptId=2\x07\x1b]633;F\x07> \x1b]633;G\x07",
    );
    submitted(&mut output, true);
    feed(
        &mut output,
        b"\x1b]633;P;ChelixPromptId=2\x07\x1b]633;F\x07> \x1b]633;G\x07",
    );
    assert_eq!(output.input_prompt, None);
    feed(
        &mut output,
        b"\x1b]633;P;ChelixPromptId=3\x07\x1b]633;F\x07> \x1b]633;G\x07",
    );
    assert_eq!(output.input_prompt, Some(PromptKind::Continuation));
    assert!(terminal_running(&output));
    feed(&mut output, b"\x1b]633;C\x07\x1b]633;D;0\x07\x1b]633;P;ChelixPromptId=1\x07\x1b]633;A\x07$ \x1b]633;B\x07");
    assert!(!terminal_running(&output));
    assert_eq!(output.prompt_id, Some(1));
}

#[tokio::test]
async fn invalid_markers_in_command_output_preserve_feeding_and_terminal_reuse() {
    let (manager, terminal) = setup().await;
    let command = format!(
        "printf '\\033]633;D;not-a-number\\007\\033]633;P;ChelixPromptId=not-a-number\\007\\033]633;P;ChelixPromptId={PROMPT_ID_EXPRESSION}\\007'; echo valid\necho next"
    );
    let response = execute(&manager, &terminal, &command).await;
    assert!(response.completed);
    assert_eq!(response.exit_code, Some(0));
    assert!(response.output.lines().any(|line| line == "valid"));
    assert!(response.output.lines().any(|line| line == "next"));
    assert!(read(&manager, &terminal).await.error.is_none());
    let reused = execute(&manager, &terminal, "echo reused").await;
    assert!(reused.completed);
    assert_eq!(reused.terminal_id, response.terminal_id);
    assert!(reused.output.lines().any(|line| line == "reused"));
    shutdown(&manager).await;
}

#[test]
fn completed_capture_requires_a_command_start_boundary() {
    let mut output = state();
    start_run(&mut output);
    output.closed = true;
    let run = output
        .active_run
        .as_mut()
        .unwrap_or_else(|| panic!("missing run"));
    run.completed = true;
    let error = run_output(
        &output,
        output
            .active_run
            .as_ref()
            .unwrap_or_else(|| panic!("missing run")),
    )
    .err()
    .unwrap_or_else(|| panic!("missing command boundary should fail"));
    assert_eq!(
        error.to_string(),
        "terminal run run completed without a command-start boundary"
    );
}

#[tokio::test]
async fn single_command_boundaries_preserve_clean_output_with_and_without_cwd() {
    let (manager, terminal) = setup().await;
    for custom_cwd in [false, true] {
        for (command, expected, status) in [
            ("echo single", "single", 0),
            ("true", "", 0),
            ("false", "", 1),
            ("cat <<'EOF'\none\ntwo\nEOF", "one\ntwo", 0),
            ("if true; then\n  echo inside\nfi", "inside", 0),
            ("echo 1; echo 2; echo 3", "1\n2\n3", 0),
        ] {
            let mut command_request = request(command, Some(&terminal.id));
            if custom_cwd {
                command_request.custom_cwd =
                    Some(std::env::temp_dir().to_string_lossy().into_owned());
            }
            let response = manager
                .execute_command(command_request)
                .await
                .unwrap_or_else(|error| panic!("single command failed: {error}"));
            assert!(response.completed, "{response:?}");
            assert_eq!(response.exit_code, Some(status), "{command:?}");
            assert_eq!(
                response.output, expected,
                "cwd={custom_cwd}, command={command:?}"
            );
            assert_eq!(
                lock(&terminal.output)
                    .active_run
                    .as_ref()
                    .unwrap_or_else(|| panic!("missing run"))
                    .command_started_count,
                1
            );
        }
    }
    shutdown(&manager).await;
}

#[tokio::test]
async fn background_snapshot_precedes_feeding_the_remaining_commands() {
    let (manager, terminal) = setup().await;
    let mut command = request("echo bg-start\nsleep 3\necho bg-finish", Some(&terminal.id));
    command.background = true;
    let response = manager
        .execute_command(command)
        .await
        .unwrap_or_else(|error| panic!("background execution failed: {error}"));
    assert!(response.background);
    assert!("bg-start".starts_with(&response.output));
    for excluded in ["sleep 3", "bg-finish", "$ "] {
        assert!(!response.output.contains(excluded));
    }
    assert!(
        wait_for_run(&terminal, &response.run_id, Duration::from_secs(5))
            .await
            .unwrap_or_else(|error| panic!("background wait failed: {error}"))
    );
    let history = read(&manager, &terminal).await;
    assert!(history.completed);
    assert_eq!(history.exit_code, Some(0));
    assert!(history.output.lines().any(|line| line == "bg-start"));
    assert!(history.output.lines().any(|line| line == "bg-finish"));
    shutdown(&manager).await;
}

#[tokio::test]
async fn disabled_prompt_expansion_reports_failure_and_preserves_direct_control() {
    let (manager, terminal) = setup().await;
    let error = manager
        .execute_command(request("shopt -u promptvars", Some(&terminal.id)))
        .await
        .err()
        .unwrap_or_else(|| panic!("disabled prompt expansion must report an error"));
    assert!(error.to_string().contains("Bash promptvars is disabled"));
    let history = read(&manager, &terminal).await;
    assert!(history.alive);
    assert!(
        history
            .error
            .as_deref()
            .is_some_and(|error| error.contains("Bash promptvars is disabled"))
    );
    assert!(history.output.contains("shopt -u promptvars"));
    manager
        .write_terminal(
            SESSION,
            &terminal.id,
            b"shopt -s promptvars; printf 'control-restored\\n'\r",
        )
        .await
        .unwrap_or_else(|error| panic!("direct control failed: {error}"));
    wait_for_state(&terminal, |output| {
        output.at_prompt
            && retained_text(output.screen.screen(), None)
                .unwrap_or_default()
                .lines()
                .any(|line| line == "control-restored")
    })
    .await;
    let restored = read(&manager, &terminal).await;
    assert!(restored.alive);
    assert!(restored.completed);
    assert_eq!(restored.error, history.error);
    shutdown(&manager).await;
}

#[tokio::test]
async fn background_and_timeout_snapshots_preserve_real_single_command_output() {
    let (manager, terminal) = setup().await;
    let mut command = request("printf 'partial\\n'; read -r value", Some(&terminal.id));
    command.background = true;
    let response = manager
        .execute_command(command)
        .await
        .unwrap_or_else(|error| panic!("background execution failed: {error}"));
    assert!(response.background);
    assert!("partial".starts_with(&response.output));
    wait_for_state(&terminal, |output| {
        retained_text(output.screen.screen(), None)
            .unwrap_or_default()
            .lines()
            .any(|line| line == "partial")
    })
    .await;
    for (timed_out, background) in [(false, true), (true, false)] {
        let snapshot = response_for_run(&terminal, &response.run_id, false, timed_out, background)
            .unwrap_or_else(|error| panic!("partial response failed: {error}"));
        assert_eq!(snapshot.output, "partial");
        assert_eq!(snapshot.timed_out, timed_out);
        assert_eq!(snapshot.background, background);
    }
    manager
        .paste_terminal_text(SESSION, &terminal.id, "\r")
        .await
        .unwrap_or_else(|error| panic!("interactive completion failed: {error}"));
    assert!(
        wait_for_run(&terminal, &response.run_id, Duration::from_secs(5))
            .await
            .unwrap_or_else(|error| panic!("single command wait failed: {error}"))
    );
    let completed = response_for_run(&terminal, &response.run_id, true, false, false)
        .unwrap_or_else(|error| panic!("completed response failed: {error}"));
    assert_eq!(completed.output, "partial");
    shutdown(&manager).await;
}

#[tokio::test]
async fn multiline_commands_return_the_complete_transcript_and_last_status() {
    let (manager, terminal) = setup().await;
    let response = execute(
        &manager,
        &terminal,
        "echo 1\necho 2\necho 3\necho 4\necho 5\nfalse",
    )
    .await;
    assert!(response.completed);
    assert!(!response.timed_out);
    assert_eq!(response.exit_code, Some(1));
    assert_eq!(
        response.output,
        "$ echo 1\n1\n$ echo 2\n2\n$ echo 3\n3\n$ echo 4\n4\n$ echo 5\n5\n$ false\n$"
    );
    assert!(
        read(&manager, &terminal)
            .await
            .output
            .ends_with(&response.output)
    );
    let compound = execute(
        &manager,
        &terminal,
        "if true; then\n  echo inside\nfi\necho after",
    )
    .await;
    assert!(compound.completed);
    assert_eq!(
        compound.output,
        "$ if true; then\n>   echo inside\n> fi\ninside\n$ echo after\nafter\n$"
    );
    shutdown(&manager).await;
}

#[tokio::test]
async fn shell_constructs_and_heredoc_bytes_work_in_both_paste_modes() {
    let (manager, terminal) = setup().await;
    for mode in ["off", "on"] {
        let configured = execute(
            &manager,
            &terminal,
            &format!("bind 'set enable-bracketed-paste {mode}'; bind '\"\\C-i\": self-insert'"),
        )
        .await;
        assert!(configured.completed);
        let multiple = execute(&manager, &terminal, "\n# comment\necho first\n\nfalse\n").await;
        assert!(multiple.completed);
        assert_eq!(multiple.exit_code, Some(1));
        assert!(multiple.output.contains("\nfirst\n"));
        for (command, expected, status) in [
            ("# only a comment", "", 1),
            ("if true; then\nprintf 'inside\\n'\nfi", "inside", 0),
            ("printf '%s\\n' one \\\ntwo", "one\ntwo", 0),
            ("printf '%s\\n' 'one\ntwo'", "one\ntwo", 0),
            (
                "cat <<'EOF' | od -An -tx1\na\n\n\tb\nEOF",
                " 61 0a 0a 09 62 0a",
                0,
            ),
            (
                "cat <<-'EOF' | od -An -tx1\n\ta\n\tb\n\tEOF",
                " 61 0a 62 0a",
                0,
            ),
        ] {
            let response = execute(&manager, &terminal, command).await;
            assert!(
                response.completed,
                "mode {mode}, command {command:?}: {response:?}"
            );
            assert_eq!(
                response.exit_code,
                Some(status),
                "mode {mode}, command {command:?}"
            );
            assert_eq!(
                response.output, expected,
                "mode {mode}, command {command:?}"
            );
        }
    }
    shutdown(&manager).await;
}

#[tokio::test]
async fn unfinished_heredoc_survives_timeout_redraw_resize_and_manual_completion() {
    let (manager, terminal) = setup().await;
    let mut command = request("cat <<'EOF'\none", Some(&terminal.id));
    command.timeout_millis = 100;
    let response = manager
        .execute_command(command)
        .await
        .unwrap_or_else(|error| panic!("heredoc execution failed: {error}"));
    assert!(response.timed_out);
    wait_for_state(&terminal, |output| {
        output.input_prompt == Some(PromptKind::Continuation)
            && output
                .active_run
                .as_ref()
                .is_some_and(|run| run.input_complete)
    })
    .await;
    let prompt_id = lock(&terminal.output).prompt_id;
    let error = manager
        .execute_command(request("echo busy", Some(&terminal.id)))
        .await
        .err()
        .unwrap_or_else(|| panic!("unfinished heredoc should be busy"));
    assert!(error.to_string().contains("unfinished shell construct"));
    let history_len = lock(&terminal.output).history.len();
    manager
        .send_terminal_keys(SESSION, &terminal.id, "C-l")
        .await
        .unwrap_or_else(|error| panic!("redraw failed: {error}"));
    wait_for_state(&terminal, |output| output.history.len() > history_len).await;
    manager
        .resize_terminal(SESSION, &terminal.id, 100, 40)
        .await
        .unwrap_or_else(|error| panic!("resize failed: {error}"));
    assert!(read(&manager, &terminal).await.running);
    assert_eq!(lock(&terminal.output).prompt_id, prompt_id);
    manager
        .paste_terminal_text(SESSION, &terminal.id, "EOF\r")
        .await
        .unwrap_or_else(|error| panic!("heredoc completion failed: {error}"));
    assert!(
        wait_for_run(&terminal, &response.run_id, Duration::from_secs(5))
            .await
            .unwrap_or_else(|error| panic!("heredoc wait failed: {error}"))
    );
    let output = read(&manager, &terminal).await;
    assert!(!output.running);
    assert!(output.output.lines().any(|line| line == "one"));
    assert!(output.alive);
    assert!(output.error.is_none());
    assert!(
        execute(&manager, &terminal, "echo reusable")
            .await
            .completed
    );
    shutdown(&manager).await;
}

#[tokio::test]
async fn foreground_input_remains_direct_and_queued_shell_lines_survive_background_and_timeout() {
    let (manager, terminal) = setup().await;
    for background in [true, false] {
        let mut command = request(
            "read -r value; printf '<%s>\\n' \"$value\"\nprintf 'after-read\\n'",
            Some(&terminal.id),
        );
        command.background = background;
        command.timeout_millis = 50;
        let response = manager
            .execute_command(command)
            .await
            .unwrap_or_else(|error| panic!("interactive execution failed: {error}"));
        assert_eq!(response.background, background);
        assert_eq!(response.timed_out, !background);
        assert_eq!(response.output, "");
        wait_for_state(&terminal, |output| {
            output
                .active_run
                .as_ref()
                .is_some_and(|run| run.command_started_count > 0)
        })
        .await;
        let busy = manager
            .execute_command(request("echo busy", Some(&terminal.id)))
            .await;
        assert!(busy.is_err());
        assert!(read(&manager, &terminal).await.running);
        if background {
            assert!(
                !read(&manager, &terminal)
                    .await
                    .output
                    .contains("$ printf 'after-read")
            );
            manager
                .paste_terminal_text(SESSION, &terminal.id, "from-process")
                .await
                .unwrap_or_else(|error| panic!("paste failed: {error}"));
            manager
                .send_terminal_keys(SESSION, &terminal.id, "Enter")
                .await
                .unwrap_or_else(|error| panic!("Enter failed: {error}"));
        } else {
            manager
                .write_terminal(SESSION, &terminal.id, b"from-websocket\r")
                .await
                .unwrap_or_else(|error| panic!("direct input failed: {error}"));
        }
        assert!(
            wait_for_run(&terminal, &response.run_id, Duration::from_secs(5))
                .await
                .unwrap_or_else(|error| panic!("interactive wait failed: {error}"))
        );
        let response = response_for_run(&terminal, &response.run_id, true, false, false)
            .unwrap_or_else(|error| panic!("run response failed: {error}"));
        let expected = if background {
            "<from-process>"
        } else {
            "<from-websocket>"
        };
        assert!(
            response.output.lines().any(|line| line == expected),
            "{:?}",
            response.output
        );
        assert!(
            response
                .output
                .contains("$ printf 'after-read\\n'\nafter-read\n$")
        );
        assert_eq!(response.exit_code, Some(0));
    }
    shutdown(&manager).await;
}

#[tokio::test]
async fn ctrl_c_releases_the_foreground_before_the_next_payload_line() {
    let (manager, terminal) = setup().await;
    let mut command = request("cat\nprintf 'after-cat\\n'", Some(&terminal.id));
    command.background = true;
    let response = manager
        .execute_command(command)
        .await
        .unwrap_or_else(|error| panic!("cat execution failed: {error}"));
    wait_for_state(&terminal, |output| {
        output
            .active_run
            .as_ref()
            .is_some_and(|run| run.command_started_count > 0)
    })
    .await;
    manager
        .write_terminal(SESSION, &terminal.id, b"interactive-cat\r")
        .await
        .unwrap_or_else(|error| panic!("cat input failed: {error}"));
    wait_for_state(&terminal, |output| {
        retained_text(output.screen.screen(), None)
            .unwrap_or_default()
            .lines()
            .filter(|line| *line == "interactive-cat")
            .count()
            == 2
    })
    .await;
    assert!(!read(&manager, &terminal).await.output.contains("after-cat"));
    manager
        .send_terminal_keys(SESSION, &terminal.id, "C-c")
        .await
        .unwrap_or_else(|error| panic!("Ctrl-C failed: {error}"));
    assert!(
        wait_for_run(&terminal, &response.run_id, Duration::from_secs(5))
            .await
            .unwrap_or_else(|error| panic!("cat wait failed: {error}"))
    );
    let output = read(&manager, &terminal).await;
    assert!(
        output
            .output
            .contains("$ printf 'after-cat\\n'\nafter-cat\n$")
    );
    assert!(output.alive);
    shutdown(&manager).await;
}

#[tokio::test]
async fn prompts_and_shell_replacement_preserve_status_and_persistent_terminal_identity() {
    let (manager, terminal) = setup().await;
    let response = execute(&manager, &terminal,
        "PS1='custom> '; PS2='more> '\nfalse\nprintf 'status=%s\\n' \"$?\"\nexec bash --noprofile --norc -i\ncat <<'EOF'\nreplaced\nEOF").await;
    assert!(response.completed, "{response:?}");
    assert_eq!(response.terminal_id, terminal.id);
    assert!(response.output.lines().any(|line| line == "status=1"));
    assert!(response.output.contains("custom> false"));
    assert!(response.output.lines().any(|line| line == "replaced"));
    let response = execute(&manager, &terminal, "unset __chelix_prompt_id\necho reset").await;
    assert!(response.completed);
    assert!(response.output.lines().any(|line| line == "reset"));
    shutdown(&manager).await;
}

#[tokio::test]
async fn shell_exit_and_kill_stop_pending_payload() {
    let (manager, terminal) = setup().await;
    let response = execute(&manager, &terminal, "exit 7\necho must-not-run").await;
    assert!(response.completed);
    assert!(!response.alive);
    assert_eq!(response.exit_code, Some(7));
    assert!(!response.output.contains("must-not-run"));
    let eof = manager
        .execute_command(request("exec printf 'eof-without-newline'", None))
        .await
        .unwrap_or_else(|error| panic!("EOF execution failed: {error}"));
    assert!(eof.completed);
    assert!(!eof.alive);
    assert_eq!(eof.output, "eof-without-newline");
    let mut command = request("read -r value\necho must-not-run", None);
    command.background = true;
    let response = manager
        .execute_command(command)
        .await
        .unwrap_or_else(|error| panic!("background read failed: {error}"));
    let terminal = manager
        .find_terminal(SESSION, &response.terminal_id)
        .await
        .unwrap_or_else(|error| panic!("terminal lookup failed: {error}"));
    wait_for_state(&terminal, |output| {
        output
            .active_run
            .as_ref()
            .is_some_and(|run| run.command_started_count > 0)
    })
    .await;
    manager
        .kill_terminal(SESSION, &terminal.id)
        .await
        .unwrap_or_else(|error| panic!("terminal kill failed: {error}"));
    wait_for_state(&terminal, |output| output.closed).await;
    assert!(!String::from_utf8_lossy(&lock(&terminal.output).history).contains("must-not-run"));
    shutdown(&manager).await;
}

#[tokio::test]
async fn feeder_survives_cancellation_of_the_request_waiter() {
    let (manager, terminal) = setup().await;
    let executing = Arc::clone(&manager);
    let command = request("read -r value\necho after-cancel", Some(&terminal.id));
    let waiter = tokio::spawn(async move { executing.execute_command(command).await });
    wait_for_state(&terminal, |output| {
        output
            .active_run
            .as_ref()
            .is_some_and(|run| !run.completed && run.command_started_count > 0)
    })
    .await;
    let run_id = lock(&terminal.output)
        .active_run
        .as_ref()
        .unwrap_or_else(|| panic!("missing run"))
        .id
        .clone();
    waiter.abort();
    assert!(waiter.await.is_err());
    manager
        .write_terminal(SESSION, &terminal.id, b"continue\r")
        .await
        .unwrap_or_else(|error| panic!("continuation input failed: {error}"));
    assert!(
        wait_for_run(&terminal, &run_id, Duration::from_secs(5))
            .await
            .unwrap_or_else(|error| panic!("run wait failed: {error}"))
    );
    assert!(
        read(&manager, &terminal)
            .await
            .output
            .lines()
            .any(|line| line == "after-cancel")
    );
    assert!(lock(&manager.tool_call_terminals).is_empty());
    shutdown(&manager).await;
}

#[tokio::test]
async fn saved_feeder_failure_preserves_history_inventory_and_direct_control() {
    let (manager, terminal) = setup().await;
    let healthy = manager
        .create_interactive_terminal("session:healthy", &[])
        .await
        .unwrap_or_else(|error| panic!("healthy terminal creation failed: {error}"));
    let mut command = request(
        "read -r value; printf '<%s>\\n' \"$value\"\necho must-not-feed",
        Some(&terminal.id),
    );
    command.background = true;
    let response = manager
        .execute_command(command)
        .await
        .unwrap_or_else(|error| panic!("read execution failed: {error}"));
    wait_for_state(&terminal, |output| {
        output
            .active_run
            .as_ref()
            .is_some_and(|run| run.command_started_count > 0)
    })
    .await;
    let error = anyhow::Error::from(std::io::Error::new(
        std::io::ErrorKind::BrokenPipe,
        "writer failed",
    ));
    record_run_error(&terminal, &response.run_id, &error);
    assert!(
        wait_for_run(&terminal, &response.run_id, Duration::from_secs(1))
            .await
            .is_err()
    );
    let history = read(&manager, &terminal).await;
    assert!(
        history
            .error
            .as_deref()
            .is_some_and(|error| error.contains("writer failed"))
    );
    assert!(history.output.contains("read -r value"));
    assert!(history.alive);
    let inventory = manager
        .terminal_infos()
        .await
        .unwrap_or_else(|error| panic!("inventory failed: {error}"));
    assert!(
        inventory
            .iter()
            .any(|info| info.id == healthy.id && info.alive)
    );
    assert!(
        inventory
            .iter()
            .any(|info| info.id == terminal.id && info.alive)
    );
    assert!(
        manager
            .execute_command(request("echo rejected", Some(&terminal.id)))
            .await
            .is_err()
    );
    let mut subscription = manager
        .subscribe_terminal(SESSION, &terminal.id)
        .await
        .unwrap_or_else(|error| panic!("subscription failed: {error}"));
    manager
        .write_terminal(SESSION, &terminal.id, b"still-interactive\r")
        .await
        .unwrap_or_else(|error| panic!("direct input failed: {error}"));
    wait_for_state(&terminal, |output| output.at_prompt).await;
    let history = read(&manager, &terminal).await;
    assert!(
        history
            .output
            .lines()
            .any(|line| line == "<still-interactive>")
    );
    assert!(!history.output.contains("must-not-feed"));
    assert!(history.error.is_some());
    assert!(
        subscription
            .next_output()
            .await
            .unwrap_or_else(|error| panic!("subscription read failed: {error}"))
            .is_some()
    );
    let limited = manager
        .read_terminal_output(ReadTerminalOutputRequest {
            session_key: SESSION.into(),
            terminal_id: terminal.id.clone(),
            max_lines: Some(1),
        })
        .await
        .unwrap_or_else(|error| panic!("limited read failed: {error}"));
    assert_eq!(limited.output, "$");
    assert_eq!(limited.error, history.error);
    shutdown(&manager).await;
}
