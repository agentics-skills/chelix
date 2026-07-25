//! Typed `ripgrep` execution for the managed tools service.
//!
//! The service validates its `rg` runtime during startup, streams the JSONL
//! protocol under one combined stdout/stderr budget, and terminates the child
//! only for an explicit limit, timeout, or processing error.

use {
    anyhow::{Context, Result, bail},
    base64::Engine as _,
    chelix_protocol::{
        RipgrepCaseMode, RipgrepContextLine, RipgrepDetail, RipgrepInput, RipgrepLimits,
        RipgrepMatch, RipgrepResult, RipgrepSubmatch, RipgrepSummary,
    },
    serde::Deserialize,
    serde_json::Value,
    std::{
        collections::HashSet,
        path::{Path, PathBuf},
        process::{ExitStatus, Stdio},
        sync::Arc,
        time::Duration,
    },
    tokio::{
        io::{AsyncRead, AsyncReadExt},
        process::{ChildStderr, ChildStdout, Command},
        sync::mpsc,
        time::{Instant, sleep_until},
    },
    tracing::instrument,
};

const STDERR_MAX_CHARS: usize = 2000;
const OUTPUT_CHUNK_BYTES: usize = 8192;

#[derive(Debug)]
pub(crate) struct RipgrepRuntime {
    working_dir: PathBuf,
    known_type_names: HashSet<String>,
}

impl RipgrepRuntime {
    pub(crate) async fn initialize(working_dir: PathBuf) -> Result<Arc<Self>> {
        validate_working_directory(&working_dir).await?;
        let known_type_names = load_type_names(&working_dir).await?;
        Ok(Arc::new(Self {
            working_dir,
            known_type_names,
        }))
    }

    #[cfg(test)]
    fn for_test(working_dir: PathBuf, known_type_names: HashSet<String>) -> Self {
        Self {
            working_dir,
            known_type_names,
        }
    }
}

async fn validate_working_directory(path: &Path) -> Result<()> {
    let metadata = match tokio::fs::metadata(path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            bail!(
                "ripgrep working directory does not exist: {}",
                path.display()
            );
        },
        Err(error) => {
            bail!(
                "failed to inspect ripgrep working directory {}: {error}",
                path.display()
            );
        },
    };
    if !metadata.is_dir() {
        bail!(
            "ripgrep working directory is not a directory: {}",
            path.display()
        );
    }
    Ok(())
}

async fn load_type_names(working_dir: &Path) -> Result<HashSet<String>> {
    let output = Command::new("rg")
        .arg("--type-list")
        .current_dir(working_dir)
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|error| anyhow::anyhow!("ripgrep executable is unavailable: {error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "rg --type-list failed with status {}: {}",
            output.status,
            stderr.trim()
        );
    }
    let stdout = String::from_utf8(output.stdout)
        .context("rg --type-list returned output that is not UTF-8")?;
    let mut names = parse_type_list(&stdout);
    // `all` is a supported rg pseudo-type but is intentionally omitted from
    // `rg --type-list` because it represents every registered type.
    names.insert("all".to_string());
    if names.len() == 1 {
        bail!("rg --type-list returned no file type definitions");
    }
    Ok(names)
}

fn parse_type_list(output: &str) -> HashSet<String> {
    output
        .lines()
        .filter_map(|line| {
            let (name, _) = line.split_once(':')?;
            let normalized = name.trim().to_ascii_lowercase();
            (!normalized.is_empty()).then_some(normalized)
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TypeFilter {
    Type(String),
    Glob(String),
}

const EXTENSION_TYPE_ALIASES: &[(&str, &str)] = &[
    ("cjs", "js"),
    ("cts", "ts"),
    ("jsx", "js"),
    ("mjs", "js"),
    ("mts", "ts"),
    ("tsx", "ts"),
];

fn is_extension_like(raw: &str) -> bool {
    let rest = raw.strip_prefix('.').unwrap_or(raw);
    let mut chars = rest.chars();
    chars
        .next()
        .is_some_and(|character| character.is_ascii_alphanumeric())
        && chars.all(|character| {
            character.is_ascii_alphanumeric() || character == '_' || character == '-'
        })
}

fn resolve_type_filter(
    raw: &str,
    known_type_names: &HashSet<String>,
    exclude: bool,
) -> Option<TypeFilter> {
    let trimmed = raw.trim();
    let normalized = trimmed
        .strip_prefix('.')
        .unwrap_or(trimmed)
        .to_ascii_lowercase();
    if normalized.is_empty() {
        return None;
    }
    if let Some((_, alias)) = EXTENSION_TYPE_ALIASES
        .iter()
        .find(|(extension, _)| *extension == normalized)
    {
        return Some(TypeFilter::Type((*alias).to_string()));
    }
    if known_type_names.contains(&normalized) {
        return Some(TypeFilter::Type(normalized));
    }
    if is_extension_like(trimmed) {
        let glob = format!("*.{normalized}");
        return Some(TypeFilter::Glob(if exclude {
            format!("!{glob}")
        } else {
            glob
        }));
    }
    Some(TypeFilter::Type(trimmed.to_string()))
}

fn collect_type_filters(
    raw_names: &[String],
    known_type_names: &HashSet<String>,
    exclude: bool,
) -> (Vec<String>, Vec<String>) {
    let mut type_names = Vec::new();
    let mut globs = Vec::new();
    for raw in raw_names {
        match resolve_type_filter(raw, known_type_names, exclude) {
            Some(TypeFilter::Type(name)) if !type_names.contains(&name) => type_names.push(name),
            Some(TypeFilter::Type(_)) | None => {},
            Some(TypeFilter::Glob(glob)) => globs.push(glob),
        }
    }
    (type_names, globs)
}

fn build_args(input: &RipgrepInput, known_type_names: &HashSet<String>) -> Vec<String> {
    let (include_types, include_globs) =
        collect_type_filters(&input.include_types, known_type_names, false);
    let (exclude_types, exclude_globs) =
        collect_type_filters(&input.type_not, known_type_names, true);

    let mut args = vec!["--json".to_string()];
    if input.fixed_strings {
        args.push("-F".to_string());
    }
    match input.case_mode {
        Some(RipgrepCaseMode::Ignore) => args.push("-i".to_string()),
        Some(RipgrepCaseMode::Smart) => args.push("-S".to_string()),
        Some(RipgrepCaseMode::Sensitive) | None => {},
    }
    if input.include_hidden {
        args.push("--hidden".to_string());
    }
    match input.unrestricted {
        1 => args.push("-u".to_string()),
        2 => args.push("-uu".to_string()),
        3 => args.push("-uuu".to_string()),
        _ => {},
    }
    if input.follow_symlinks {
        args.push("--follow".to_string());
    }
    if let Some(context) = input.context_lines {
        args.push("-C".to_string());
        args.push(context.to_string());
    }
    for glob in include_globs.iter().chain(&exclude_globs) {
        args.push("--glob".to_string());
        args.push(glob.clone());
    }
    for glob in input.glob.iter().filter(|glob| !glob.is_empty()) {
        args.push("--glob".to_string());
        args.push(glob.clone());
    }
    for name in include_types {
        args.push("--type".to_string());
        args.push(name);
    }
    for name in exclude_types {
        args.push("--type-not".to_string());
        args.push(name);
    }
    args.push("--".to_string());
    args.push(input.pattern.clone());
    args.extend(input.paths.iter().cloned());
    args
}

#[derive(Debug, Deserialize)]
struct RgText {
    text: Option<String>,
    bytes: Option<String>,
}

impl RgText {
    fn decode(&self) -> Result<String> {
        if let Some(text) = &self.text {
            return Ok(text.clone());
        }
        let bytes = self
            .bytes
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("rg JSON payload has neither 'text' nor 'bytes'"))?;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(bytes)
            .context("invalid base64 in rg JSON output")?;
        Ok(String::from_utf8_lossy(&decoded).into_owned())
    }
}

#[derive(Debug, Deserialize)]
struct RgSubmatchData {
    #[serde(rename = "match")]
    matched: RgText,
    start: u64,
    end: u64,
}

#[derive(Debug, Deserialize)]
struct RgMatchData {
    path: RgText,
    lines: RgText,
    line_number: Option<u64>,
    #[serde(default)]
    submatches: Vec<RgSubmatchData>,
}

#[derive(Debug, Deserialize)]
struct RgContextData {
    path: RgText,
    lines: RgText,
    line_number: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct RgSummaryData {
    stats: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "lowercase")]
enum RgMessage {
    Begin(serde::de::IgnoredAny),
    Match(RgMatchData),
    Context(RgContextData),
    End(serde::de::IgnoredAny),
    Summary(RgSummaryData),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    Continue,
    Stop,
}

struct ScanState {
    detail: RipgrepDetail,
    max_matches: usize,
    max_files: usize,
    seen_files: HashSet<String>,
    files: Vec<String>,
    matches: Vec<RipgrepMatch>,
    context: Vec<RipgrepContextLine>,
    match_count: usize,
    truncated_reason: Option<String>,
    stats: Option<Value>,
}

impl ScanState {
    fn new(input: &RipgrepInput) -> Self {
        Self {
            detail: input.detail,
            max_matches: input.max_matches,
            max_files: input.max_files,
            seen_files: HashSet::new(),
            files: Vec::new(),
            matches: Vec::new(),
            context: Vec::new(),
            match_count: 0,
            truncated_reason: None,
            stats: None,
        }
    }

    fn wants_rows(&self) -> bool {
        matches!(
            self.detail,
            RipgrepDetail::Lines | RipgrepDetail::LinesSubmatches
        )
    }

    fn truncate(&mut self, reason: &str) -> Flow {
        self.truncated_reason = Some(reason.to_string());
        Flow::Stop
    }

    fn process_line(&mut self, line: &str) -> Result<Flow> {
        if line.trim().is_empty() {
            return Ok(Flow::Continue);
        }
        let message: RgMessage = serde_json::from_str(line)
            .with_context(|| format!("rg JSON parse error for line {line:?}"))?;
        match message {
            RgMessage::Match(data) => self.process_match(&data),
            RgMessage::Context(data) => self.process_context(&data),
            RgMessage::Summary(data) => {
                self.stats = data.stats;
                Ok(Flow::Continue)
            },
            RgMessage::Begin(_) | RgMessage::End(_) => Ok(Flow::Continue),
        }
    }

    fn process_match(&mut self, data: &RgMatchData) -> Result<Flow> {
        let path = data.path.decode()?;
        if !self.seen_files.contains(&path) {
            if self.seen_files.len() + 1 > self.max_files {
                return Ok(self.truncate("maxFiles"));
            }
            self.seen_files.insert(path.clone());
            if self.detail == RipgrepDetail::Files {
                self.files.push(path.clone());
            }
        }

        self.match_count += 1;
        if self.wants_rows()
            && let Some(line_number) = data.line_number
        {
            let submatches = if self.detail == RipgrepDetail::LinesSubmatches {
                let rows = data
                    .submatches
                    .iter()
                    .map(|submatch| {
                        Ok(RipgrepSubmatch {
                            matched: submatch.matched.decode()?,
                            start: submatch.start,
                            end: submatch.end,
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                (!rows.is_empty()).then_some(rows)
            } else {
                None
            };
            self.matches.push(RipgrepMatch {
                path,
                line_number,
                lines: data.lines.decode()?,
                submatches,
            });
        }
        if self.match_count >= self.max_matches {
            return Ok(self.truncate("maxMatches"));
        }
        Ok(Flow::Continue)
    }

    fn process_context(&mut self, data: &RgContextData) -> Result<Flow> {
        if !self.wants_rows() {
            return Ok(Flow::Continue);
        }
        let Some(line_number) = data.line_number else {
            return Ok(Flow::Continue);
        };
        self.context.push(RipgrepContextLine {
            path: data.path.decode()?,
            line_number,
            lines: data.lines.decode()?,
        });
        Ok(Flow::Continue)
    }
}

#[derive(Debug, Clone, Copy)]
enum OutputStream {
    Stdout,
    Stderr,
}

struct OutputChunk {
    stream: OutputStream,
    bytes: Vec<u8>,
}

async fn pump_output<R>(
    mut reader: R,
    stream: OutputStream,
    sender: mpsc::Sender<OutputChunk>,
) -> std::io::Result<()>
where
    R: AsyncRead + Unpin,
{
    let mut buffer = vec![0_u8; OUTPUT_CHUNK_BYTES];
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            return Ok(());
        }
        if sender
            .send(OutputChunk {
                stream,
                bytes: buffer[..read].to_vec(),
            })
            .await
            .is_err()
        {
            return Ok(());
        }
    }
}

struct OutputCollector {
    max_output_chars: usize,
    output_chars: usize,
    stdout_buffer: Vec<u8>,
    stderr: String,
    stderr_truncated: bool,
}

impl OutputCollector {
    fn new(max_output_chars: usize) -> Self {
        Self {
            max_output_chars,
            output_chars: 0,
            stdout_buffer: Vec::new(),
            stderr: String::new(),
            stderr_truncated: false,
        }
    }

    fn process_chunk(&mut self, chunk: OutputChunk, state: &mut ScanState) -> Result<Flow> {
        let chunk_chars = String::from_utf8_lossy(&chunk.bytes).encode_utf16().count();
        if self.output_chars.saturating_add(chunk_chars) > self.max_output_chars {
            return Ok(state.truncate("maxOutputChars"));
        }
        self.output_chars += chunk_chars;

        match chunk.stream {
            OutputStream::Stdout => self.process_stdout(&chunk.bytes, state),
            OutputStream::Stderr => {
                self.append_stderr(&chunk.bytes);
                Ok(Flow::Continue)
            },
        }
    }

    fn process_stdout(&mut self, bytes: &[u8], state: &mut ScanState) -> Result<Flow> {
        self.stdout_buffer.extend_from_slice(bytes);
        while let Some(index) = self.stdout_buffer.iter().position(|byte| *byte == b'\n') {
            let mut raw_line = self.stdout_buffer.drain(..=index).collect::<Vec<_>>();
            raw_line.pop();
            if raw_line.last() == Some(&b'\r') {
                raw_line.pop();
            }
            let line = std::str::from_utf8(&raw_line).context("rg JSON output is not UTF-8")?;
            if state.process_line(line)? == Flow::Stop {
                self.stdout_buffer.clear();
                return Ok(Flow::Stop);
            }
        }
        Ok(Flow::Continue)
    }

    fn finish_stdout(&mut self, state: &mut ScanState) -> Result<Flow> {
        if self.stdout_buffer.is_empty() {
            return Ok(Flow::Continue);
        }
        let raw_line = std::mem::take(&mut self.stdout_buffer);
        let line = std::str::from_utf8(&raw_line).context("rg JSON output is not UTF-8")?;
        state.process_line(line)
    }

    fn append_stderr(&mut self, bytes: &[u8]) {
        let text = String::from_utf8_lossy(bytes);
        let current_chars = self.stderr.encode_utf16().count();
        if current_chars >= STDERR_MAX_CHARS {
            self.stderr_truncated = true;
            return;
        }
        let remaining = STDERR_MAX_CHARS - current_chars;
        let mut appended = String::new();
        let mut used = 0;
        for character in text.chars() {
            let width = character.len_utf16();
            if used + width > remaining {
                self.stderr_truncated = true;
                break;
            }
            appended.push(character);
            used += width;
        }
        if used < text.encode_utf16().count() {
            self.stderr_truncated = true;
        }
        self.stderr.push_str(&appended);
    }

    fn stderr_text(&self) -> Option<String> {
        if self.stderr.is_empty() {
            return None;
        }
        Some(if self.stderr_truncated {
            format!("{}…", self.stderr)
        } else {
            self.stderr.clone()
        })
    }
}

async fn effective_working_directory(
    input: &RipgrepInput,
    runtime: &RipgrepRuntime,
) -> Result<PathBuf> {
    let working_dir = input
        .cwd
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| runtime.working_dir.clone());
    validate_working_directory(&working_dir).await?;
    Ok(working_dir)
}

async fn join_reader(name: &str, task: tokio::task::JoinHandle<std::io::Result<()>>) -> Result<()> {
    task.await
        .with_context(|| format!("rg {name} reader task failed"))?
        .with_context(|| format!("failed to read rg {name}"))
}

async fn terminate_and_wait(child: &mut tokio::process::Child) -> Result<ExitStatus> {
    if let Some(status) = child
        .try_wait()
        .context("failed to inspect rg process before termination")?
    {
        return Ok(status);
    }
    if let Err(kill_error) = child.start_kill() {
        if let Some(status) = child
            .try_wait()
            .context("failed to inspect rg process after termination race")?
        {
            return Ok(status);
        }
        return Err(kill_error).context("failed to terminate rg process");
    }
    child.wait().await.context("failed to wait for rg process")
}

fn finish_process_cleanup(
    processing_error: Option<anyhow::Error>,
    status_result: Result<ExitStatus>,
    stdout_result: Result<()>,
    stderr_result: Result<()>,
) -> Result<ExitStatus> {
    let mut status = None;
    let mut errors = Vec::new();
    if let Some(error) = processing_error {
        errors.push(format!("{error:#}"));
    }
    match status_result {
        Ok(value) => status = Some(value),
        Err(error) => errors.push(format!("rg process cleanup failed: {error:#}")),
    }
    if let Err(error) = stdout_result {
        errors.push(format!("stdout cleanup failed: {error:#}"));
    }
    if let Err(error) = stderr_result {
        errors.push(format!("stderr cleanup failed: {error:#}"));
    }
    if !errors.is_empty() {
        bail!(errors.join("; "));
    }
    status.ok_or_else(|| anyhow::anyhow!("rg process cleanup completed without an exit status"))
}

#[instrument(skip(input, runtime), fields(pattern = %input.pattern))]
async fn run_search(input: &RipgrepInput, runtime: &RipgrepRuntime) -> Result<RipgrepResult> {
    input
        .validate()
        .map_err(|error| anyhow::anyhow!("invalid ripgrep parameters: {error}"))?;
    let working_dir = effective_working_directory(input, runtime).await?;
    let args = build_args(input, &runtime.known_type_names);
    let mut child = Command::new("rg")
        .args(&args)
        .current_dir(&working_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| anyhow::anyhow!("failed to start rg executable: {error}"))?;
    let stdout: ChildStdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("rg stdout pipe missing"))?;
    let stderr: ChildStderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("rg stderr pipe missing"))?;

    let (sender, mut receiver) = mpsc::channel(8);
    let stdout_task = tokio::spawn(pump_output(stdout, OutputStream::Stdout, sender.clone()));
    let stderr_task = tokio::spawn(pump_output(stderr, OutputStream::Stderr, sender));
    let deadline = Instant::now()
        .checked_add(Duration::from_millis(input.timeout_ms))
        .ok_or_else(|| anyhow::anyhow!("ripgrep timeout is too large"))?;
    let timeout = sleep_until(deadline);
    tokio::pin!(timeout);

    let mut state = ScanState::new(input);
    let mut output = OutputCollector::new(input.max_output_chars);
    let mut stop_requested = false;
    let mut timed_out = false;
    let mut processing_error = None;
    loop {
        tokio::select! {
            biased;
            () = &mut timeout => {
                timed_out = true;
                state.truncate("timeout");
                stop_requested = true;
                break;
            },
            chunk = receiver.recv() => {
                let Some(chunk) = chunk else {
                    break;
                };
                match output.process_chunk(chunk, &mut state) {
                    Ok(Flow::Continue) => {},
                    Ok(Flow::Stop) => {
                        stop_requested = true;
                        break;
                    },
                    Err(error) => {
                        processing_error = Some(error);
                        stop_requested = true;
                        break;
                    },
                }
            },
        }
    }

    drop(receiver);
    let status_result = if stop_requested {
        terminate_and_wait(&mut child).await
    } else {
        match tokio::time::timeout_at(deadline, child.wait()).await {
            Ok(status) => status.context("failed to wait for rg process"),
            Err(_) => {
                timed_out = true;
                state.truncate("timeout");
                terminate_and_wait(&mut child).await
            },
        }
    };
    drop(child);
    let stdout_result = join_reader("stdout", stdout_task).await;
    let stderr_result = join_reader("stderr", stderr_task).await;
    let status = finish_process_cleanup(
        processing_error,
        status_result,
        stdout_result,
        stderr_result,
    )?;

    if !stop_requested && !timed_out {
        output.finish_stdout(&mut state)?;
    }

    let truncated = state.truncated_reason.is_some();
    if !truncated {
        match status.code() {
            Some(0 | 1) => {},
            Some(code) => {
                let suffix = output
                    .stderr_text()
                    .map(|stderr| format!(" stderr: {stderr}"))
                    .unwrap_or_default();
                bail!("rg failed with exit code {code}.{suffix}");
            },
            None => bail!("rg terminated without an exit code"),
        }
    }

    let elapsed = state
        .stats
        .as_ref()
        .and_then(|stats| stats.get("elapsed"))
        .cloned();
    let wants_rows = state.wants_rows();
    Ok(RipgrepResult {
        tool: "ripgrep".to_string(),
        detail: state.detail,
        found: state.match_count > 0,
        timed_out,
        truncated,
        truncated_reason: state.truncated_reason,
        limits: RipgrepLimits {
            max_matches: input.max_matches,
            max_files: input.max_files,
            max_output_chars: input.max_output_chars,
            timeout_ms: input.timeout_ms,
        },
        summary: RipgrepSummary {
            files_with_matches: state.seen_files.len(),
            match_count: state.match_count,
            elapsed,
            stats: state.stats,
        },
        files: (state.detail == RipgrepDetail::Files).then_some(state.files),
        matches: wants_rows.then_some(state.matches),
        context: wants_rows.then_some(state.context),
        stderr: output.stderr_text(),
        exit_code: status.code(),
    })
}

pub(crate) async fn run_tool(
    input: RipgrepInput,
    runtime: &RipgrepRuntime,
) -> Result<RipgrepResult> {
    run_search(&input, runtime).await
}

#[allow(clippy::unwrap_used, clippy::expect_used)]
#[cfg(test)]
mod tests {
    use {super::*, serde_json::json};

    fn input(value: Value) -> RipgrepInput {
        serde_json::from_value(value).unwrap()
    }

    fn known_type_names(names: &[&str]) -> HashSet<String> {
        names.iter().map(|name| (*name).to_string()).collect()
    }

    fn runtime(path: &Path) -> RipgrepRuntime {
        RipgrepRuntime::for_test(
            path.to_path_buf(),
            known_type_names(&["all", "docker", "js", "rust", "ts"]),
        )
    }

    #[test]
    fn parses_dynamic_type_list() {
        let names = parse_type_list("docker: Dockerfile\njsonl: *.jsonl\nsvelte: *.svelte\n");
        assert_eq!(names.len(), 3);
        assert!(names.contains("docker"));
        assert!(names.contains("jsonl"));
        assert!(names.contains("svelte"));
    }

    #[test]
    fn build_args_uses_runtime_types_and_extension_fallbacks() {
        let input = input(json!({
            "pattern": "needle",
            "type": ["docker", "tsx", "customext"],
            "typeNot": ["jsx", "otherext"]
        }));
        assert_eq!(
            build_args(&input, &known_type_names(&["all", "docker", "js", "ts"])),
            vec![
                "--json",
                "--hidden",
                "-uuu",
                "--glob",
                "*.customext",
                "--glob",
                "!*.otherext",
                "--type",
                "docker",
                "--type",
                "ts",
                "--type-not",
                "js",
                "--",
                "needle",
            ]
        );
    }

    #[test]
    fn build_args_preserves_all_flags() {
        let input = input(json!({
            "pattern": "needle",
            "paths": ["src", "docs"],
            "fixedStrings": true,
            "caseMode": "ignore",
            "includeHidden": false,
            "unrestricted": 1,
            "followSymlinks": true,
            "contextLines": 2,
            "glob": ["*.rs", ""]
        }));
        assert_eq!(
            build_args(&input, &known_type_names(&["all", "rust"])),
            vec![
                "--json", "-F", "-i", "-u", "--follow", "-C", "2", "--glob", "*.rs", "--",
                "needle", "src", "docs",
            ]
        );
    }

    #[test]
    fn scan_state_enforces_match_and_file_limits() {
        let mut state = ScanState::new(&input(json!({ "pattern": "x", "maxMatches": 1 })));
        let line = json!({
            "type": "match",
            "data": {
                "path": { "text": "a.rs" },
                "lines": { "text": "x\n" },
                "line_number": 1,
                "submatches": []
            }
        })
        .to_string();
        assert_eq!(state.process_line(&line).unwrap(), Flow::Stop);
        assert_eq!(state.truncated_reason.as_deref(), Some("maxMatches"));
        assert_eq!(state.match_count, 1);

        let mut state = ScanState::new(&input(json!({ "pattern": "x", "maxFiles": 1 })));
        let first = line;
        let second = json!({
            "type": "match",
            "data": {
                "path": { "text": "b.rs" },
                "lines": { "text": "x\n" },
                "line_number": 1,
                "submatches": []
            }
        })
        .to_string();
        assert_eq!(state.process_line(&first).unwrap(), Flow::Continue);
        assert_eq!(state.process_line(&second).unwrap(), Flow::Stop);
        assert_eq!(state.truncated_reason.as_deref(), Some("maxFiles"));
        assert_eq!(state.match_count, 1);
    }

    #[test]
    fn output_collector_enforces_combined_budget_before_buffering() {
        let input = input(json!({ "pattern": "x", "maxOutputChars": 5 }));
        let mut state = ScanState::new(&input);
        let mut collector = OutputCollector::new(input.max_output_chars);
        assert_eq!(
            collector
                .process_chunk(
                    OutputChunk {
                        stream: OutputStream::Stderr,
                        bytes: b"abc".to_vec(),
                    },
                    &mut state,
                )
                .unwrap(),
            Flow::Continue
        );
        assert_eq!(
            collector
                .process_chunk(
                    OutputChunk {
                        stream: OutputStream::Stdout,
                        bytes: b"def".to_vec(),
                    },
                    &mut state,
                )
                .unwrap(),
            Flow::Stop
        );
        assert!(collector.stdout_buffer.is_empty());
        assert_eq!(state.truncated_reason.as_deref(), Some("maxOutputChars"));
    }

    #[test]
    fn output_collector_processes_final_unterminated_json_line() {
        let input = input(json!({ "pattern": "x" }));
        let mut state = ScanState::new(&input);
        let mut collector = OutputCollector::new(input.max_output_chars);
        let summary = json!({
            "type": "summary",
            "data": { "stats": { "matches": 0 } }
        })
        .to_string();
        collector
            .process_stdout(summary.as_bytes(), &mut state)
            .unwrap();
        collector.finish_stdout(&mut state).unwrap();
        assert_eq!(state.stats.unwrap()["matches"], 0);
    }

    async fn setup_tree() -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        tokio::fs::write(
            directory.path().join("alpha.txt"),
            "first line\nripgrep-needle here\nlast line\n",
        )
        .await
        .unwrap();
        tokio::fs::write(
            directory.path().join("beta.txt"),
            "ripgrep-needle one\nripgrep-needle two\n",
        )
        .await
        .unwrap();
        tokio::fs::write(directory.path().join("Dockerfile"), "ripgrep-needle\n")
            .await
            .unwrap();
        directory
    }

    #[tokio::test]
    async fn initialize_loads_real_rg_type_names() {
        let directory = tempfile::tempdir().unwrap();
        let runtime = RipgrepRuntime::initialize(directory.path().to_path_buf())
            .await
            .unwrap();
        assert!(runtime.known_type_names.contains("docker"));
        assert!(runtime.known_type_names.contains("jsonl"));
        assert!(runtime.known_type_names.contains("all"));
    }

    #[tokio::test]
    async fn search_uses_explicit_default_working_directory() {
        let directory = setup_tree().await;
        let result = run_tool(
            input(json!({
                "pattern": "ripgrep-needle",
                "fixedStrings": true,
                "detail": "summary"
            })),
            &runtime(directory.path()),
        )
        .await
        .unwrap();

        assert!(result.found);
        assert_eq!(result.summary.match_count, 4);
        assert_eq!(result.summary.files_with_matches, 3);
        assert!(result.files.is_none());
        assert!(result.matches.is_none());
        assert!(result.context.is_none());
        assert_eq!(result.exit_code, Some(0));
    }

    #[tokio::test]
    async fn search_resolves_paths_relative_to_explicit_cwd() {
        let directory = setup_tree().await;
        let nested = directory.path().join("nested");
        tokio::fs::create_dir(&nested).await.unwrap();
        tokio::fs::write(nested.join("sample.txt"), "relative-needle\n")
            .await
            .unwrap();
        let result = run_tool(
            input(json!({
                "pattern": "relative-needle",
                "fixedStrings": true,
                "detail": "files",
                "cwd": directory.path(),
                "paths": ["nested/sample.txt"]
            })),
            &runtime(directory.path()),
        )
        .await
        .unwrap();

        assert_eq!(result.files.unwrap(), vec!["nested/sample.txt"]);
    }

    #[tokio::test]
    async fn search_accepts_one_absolute_file_path() {
        let directory = setup_tree().await;
        let result = run_tool(
            input(json!({
                "pattern": "ripgrep-needle here",
                "fixedStrings": true,
                "paths": [directory.path().join("alpha.txt")]
            })),
            &runtime(directory.path()),
        )
        .await
        .unwrap();

        assert_eq!(result.summary.match_count, 1);
        let matches = result.matches.unwrap();
        assert_eq!(matches[0].line_number, 2);
        assert!(matches[0].submatches.is_none());
    }

    #[tokio::test]
    async fn lines_with_submatches_returns_typed_submatch_rows() {
        let directory = setup_tree().await;
        let result = run_tool(
            input(json!({
                "pattern": "ripgrep-needle",
                "fixedStrings": true,
                "detail": "lines+submatches",
                "paths": ["alpha.txt"]
            })),
            &runtime(directory.path()),
        )
        .await
        .unwrap();

        let matches = result.matches.unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].path, "alpha.txt");
        assert_eq!(matches[0].line_number, 2);
        assert_eq!(matches[0].lines, "ripgrep-needle here\n");
        assert_eq!(
            matches[0].submatches.as_deref(),
            Some(
                [RipgrepSubmatch {
                    matched: "ripgrep-needle".into(),
                    start: 0,
                    end: 14,
                }]
                .as_slice()
            )
        );
    }

    #[tokio::test]
    async fn lines_detail_returns_context_rows() {
        let directory = setup_tree().await;
        let result = run_tool(
            input(json!({
                "pattern": "ripgrep-needle",
                "fixedStrings": true,
                "detail": "lines",
                "contextLines": 1,
                "paths": ["alpha.txt"]
            })),
            &runtime(directory.path()),
        )
        .await
        .unwrap();

        let context = result.context.unwrap();
        assert_eq!(context.len(), 2);
        assert_eq!(context[0].line_number, 1);
        assert_eq!(context[0].lines, "first line\n");
        assert_eq!(context[1].line_number, 3);
        assert_eq!(context[1].lines, "last line\n");
    }

    #[tokio::test]
    async fn dynamic_docker_type_finds_dockerfile() {
        let directory = setup_tree().await;
        let runtime = RipgrepRuntime::initialize(directory.path().to_path_buf())
            .await
            .unwrap();
        let result = run_tool(
            input(json!({
                "pattern": "ripgrep-needle",
                "fixedStrings": true,
                "detail": "files",
                "type": ["docker"]
            })),
            &runtime,
        )
        .await
        .unwrap();

        assert_eq!(result.files.unwrap(), vec!["Dockerfile"]);
    }

    #[tokio::test]
    async fn dynamic_extension_types_find_jsonl_and_svelte_files() {
        let directory = setup_tree().await;
        tokio::fs::write(
            directory.path().join("events.jsonl"),
            "dynamic-type-needle\n",
        )
        .await
        .unwrap();
        tokio::fs::write(
            directory.path().join("Widget.svelte"),
            "dynamic-type-needle\n",
        )
        .await
        .unwrap();
        let runtime = RipgrepRuntime::initialize(directory.path().to_path_buf())
            .await
            .unwrap();

        for (type_name, expected_file) in [("jsonl", "events.jsonl"), ("svelte", "Widget.svelte")] {
            let result = run_tool(
                input(json!({
                    "pattern": "dynamic-type-needle",
                    "fixedStrings": true,
                    "detail": "files",
                    "type": [type_name]
                })),
                &runtime,
            )
            .await
            .unwrap();
            assert_eq!(result.files.unwrap(), vec![expected_file]);
        }
    }

    #[tokio::test]
    async fn type_not_excludes_runtime_file_type() {
        let directory = setup_tree().await;
        let runtime = RipgrepRuntime::initialize(directory.path().to_path_buf())
            .await
            .unwrap();
        let result = run_tool(
            input(json!({
                "pattern": "ripgrep-needle",
                "fixedStrings": true,
                "detail": "files",
                "typeNot": ["docker"]
            })),
            &runtime,
        )
        .await
        .unwrap();

        let mut files = result.files.unwrap();
        files.sort();
        assert_eq!(files, vec!["alpha.txt", "beta.txt"]);
    }

    #[tokio::test]
    async fn search_reports_no_match_without_error() {
        let directory = setup_tree().await;
        let result = run_tool(
            input(json!({
                "pattern": "definitely-absent",
                "fixedStrings": true
            })),
            &runtime(directory.path()),
        )
        .await
        .unwrap();

        assert!(!result.found);
        assert_eq!(result.exit_code, Some(1));
        assert_eq!(result.summary.match_count, 0);
    }

    #[tokio::test]
    async fn search_rejects_file_valued_working_directory_precisely() {
        let directory = setup_tree().await;
        let file = directory.path().join("alpha.txt");
        let error = run_tool(
            input(json!({ "pattern": "x", "cwd": file })),
            &runtime(directory.path()),
        )
        .await
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("working directory is not a directory")
        );
        assert!(error.to_string().contains("alpha.txt"));
    }

    #[tokio::test]
    async fn search_rejects_missing_working_directory_precisely() {
        let directory = setup_tree().await;
        let missing = directory.path().join("missing-directory");
        let error = run_tool(
            input(json!({ "pattern": "x", "cwd": missing })),
            &runtime(directory.path()),
        )
        .await
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("working directory does not exist")
        );
        assert!(error.to_string().contains("missing-directory"));
    }

    #[tokio::test]
    async fn search_returns_real_match_limit_state() {
        let directory = setup_tree().await;
        let result = run_tool(
            input(json!({
                "pattern": "ripgrep-needle",
                "fixedStrings": true,
                "paths": ["beta.txt"],
                "maxMatches": 1
            })),
            &runtime(directory.path()),
        )
        .await
        .unwrap();

        assert!(result.found);
        assert!(result.truncated);
        assert_eq!(result.truncated_reason.as_deref(), Some("maxMatches"));
        assert_eq!(result.summary.match_count, 1);
        assert_eq!(result.matches.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn search_returns_real_file_limit_state() {
        let directory = setup_tree().await;
        let result = run_tool(
            input(json!({
                "pattern": "ripgrep-needle",
                "fixedStrings": true,
                "detail": "files",
                "paths": ["alpha.txt", "Dockerfile"],
                "maxFiles": 1
            })),
            &runtime(directory.path()),
        )
        .await
        .unwrap();

        assert!(result.found);
        assert!(result.truncated);
        assert_eq!(result.truncated_reason.as_deref(), Some("maxFiles"));
        assert_eq!(result.summary.files_with_matches, 1);
        assert_eq!(result.summary.match_count, 1);
        assert_eq!(result.files.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn search_returns_real_combined_output_limit_state() {
        let directory = setup_tree().await;
        let result = run_tool(
            input(json!({
                "pattern": "ripgrep-needle",
                "fixedStrings": true,
                "paths": ["alpha.txt"],
                "maxOutputChars": 1
            })),
            &runtime(directory.path()),
        )
        .await
        .unwrap();

        assert!(result.truncated);
        assert_eq!(result.truncated_reason.as_deref(), Some("maxOutputChars"));
        assert_eq!(result.summary.match_count, 0);
    }

    #[tokio::test]
    async fn search_surfaces_invalid_regex() {
        let directory = setup_tree().await;
        let error = run_tool(
            input(json!({ "pattern": "(unbalanced" })),
            &runtime(directory.path()),
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("exit code 2"));
        assert!(error.to_string().contains("regex parse error"));
    }

    #[tokio::test]
    async fn zero_timeout_returns_real_timeout_state() {
        let directory = setup_tree().await;
        let result = run_tool(
            input(json!({ "pattern": "needle", "timeoutMs": 0 })),
            &runtime(directory.path()),
        )
        .await
        .unwrap();

        assert!(result.timed_out);
        assert!(result.truncated);
        assert_eq!(result.truncated_reason.as_deref(), Some("timeout"));
    }
}
