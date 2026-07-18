//! `ripgrep` tool — project-wide search by shelling out to the system `rg` binary.
//!
//! Spawns `rg --json`, streams the JSON-lines protocol, and enforces
//! match/file/output/timeout limits by killing the child process as soon as a
//! limit is exceeded. The `rg` binary is assumed to be installed — a spawn
//! failure or a non-search rg failure surfaces as a tool error.

use {
    async_trait::async_trait,
    base64::Engine as _,
    chelix_agents::tool_registry::AgentTool,
    serde::{Deserialize, Serialize, de::IgnoredAny},
    serde_json::{Value, json},
    std::{collections::HashSet, process::Stdio, time::Duration},
    tokio::{
        io::{AsyncBufReadExt, AsyncReadExt, BufReader},
        process::{ChildStderr, ChildStdout, Command},
    },
    tracing::instrument,
};

#[cfg(feature = "metrics")]
use chelix_metrics::{counter, labels, tools as tools_metrics};

use crate::{Result, error::Error, params::without_null_params};

const DEFAULT_MAX_MATCHES: usize = 2000;
const DEFAULT_MAX_FILES: usize = 200;
const DEFAULT_MAX_OUTPUT_CHARS: usize = 200_000;
const DEFAULT_TIMEOUT_MS: u64 = 10_000;
const STDERR_MAX_CHARS: usize = 2000;

/// rg file type names accepted as-is (subset of `rg --type-list`).
const KNOWN_TYPE_NAMES: &[&str] = &[
    "all",
    "c",
    "cpp",
    "cs",
    "css",
    "go",
    "h",
    "html",
    "java",
    "js",
    "json",
    "markdown",
    "md",
    "php",
    "py",
    "python",
    "ruby",
    "rust",
    "sh",
    "toml",
    "ts",
    "txt",
    "typescript",
    "xml",
    "yaml",
];

/// Extension spellings normalized to canonical rg type names.
const EXTENSION_TYPE_ALIASES: &[(&str, &str)] = &[
    ("cjs", "js"),
    ("cts", "ts"),
    ("jsx", "js"),
    ("mjs", "js"),
    ("mts", "ts"),
    ("tsx", "ts"),
];

/// Project-wide search tool backed by the system `rg` binary.
#[derive(Default)]
pub struct RipgrepTool;

impl RipgrepTool {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum CaseMode {
    Sensitive,
    Ignore,
    Smart,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
enum Detail {
    #[serde(rename = "summary")]
    Summary,
    #[serde(rename = "files")]
    Files,
    #[default]
    #[serde(rename = "lines")]
    Lines,
    #[serde(rename = "lines+submatches")]
    LinesSubmatches,
}

impl Detail {
    fn wants_rows(self) -> bool {
        matches!(self, Self::Lines | Self::LinesSubmatches)
    }
}

fn default_max_matches() -> usize {
    DEFAULT_MAX_MATCHES
}

fn default_max_files() -> usize {
    DEFAULT_MAX_FILES
}

fn default_max_output_chars() -> usize {
    DEFAULT_MAX_OUTPUT_CHARS
}

fn default_timeout_ms() -> u64 {
    DEFAULT_TIMEOUT_MS
}

fn default_true() -> bool {
    true
}

fn default_unrestricted() -> u8 {
    3
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RipgrepInput {
    pattern: String,
    #[serde(default)]
    paths: Vec<String>,
    cwd: Option<String>,
    #[serde(default)]
    fixed_strings: bool,
    case_mode: Option<CaseMode>,
    #[serde(default)]
    detail: Detail,
    #[serde(default)]
    glob: Vec<String>,
    #[serde(default, rename = "type")]
    include_types: Vec<String>,
    #[serde(default)]
    type_not: Vec<String>,
    context_lines: Option<u64>,
    #[serde(default = "default_max_matches")]
    max_matches: usize,
    #[serde(default = "default_max_files")]
    max_files: usize,
    #[serde(default = "default_max_output_chars")]
    max_output_chars: usize,
    #[serde(default = "default_timeout_ms")]
    timeout_ms: u64,
    #[serde(default = "default_true")]
    include_hidden: bool,
    #[serde(default = "default_unrestricted")]
    unrestricted: u8,
    #[serde(default)]
    follow_symlinks: bool,
}

fn validate_input(input: &RipgrepInput) -> Result<()> {
    if input.pattern.is_empty() {
        return Err(Error::message("'pattern' must not be empty"));
    }
    if input.max_matches == 0 {
        return Err(Error::message("'maxMatches' must be at least 1"));
    }
    if input.max_files == 0 {
        return Err(Error::message("'maxFiles' must be at least 1"));
    }
    if input.max_output_chars == 0 {
        return Err(Error::message("'maxOutputChars' must be at least 1"));
    }
    if input.unrestricted > 3 {
        return Err(Error::message("'unrestricted' must be between 0 and 3"));
    }
    Ok(())
}

/// A resolved `type`/`typeNot` entry: either a real rg type name or a glob.
#[derive(Debug, Clone, PartialEq, Eq)]
enum TypeFilter {
    Type(String),
    Glob(String),
}

/// True when the raw value looks like a bare file extension (`ts`, `.tsx`).
fn is_extension_like(raw: &str) -> bool {
    let rest = raw.strip_prefix('.').unwrap_or(raw);
    let mut chars = rest.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Resolve one raw type entry into a `--type`/`--type-not` name or a glob.
///
/// Aliases and known type names pass through as rg types. Other
/// extension-like values become glob filters. Anything else is handed to rg
/// verbatim so that rg itself rejects unknown types.
fn resolve_type_filter(raw: &str, exclude: bool) -> Option<TypeFilter> {
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
        .find(|(ext, _)| *ext == normalized)
    {
        return Some(TypeFilter::Type((*alias).to_string()));
    }
    if KNOWN_TYPE_NAMES.contains(&normalized.as_str()) {
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

fn collect_type_filters(raw_names: &[String], exclude: bool) -> (Vec<String>, Vec<String>) {
    let mut type_names = Vec::new();
    let mut globs = Vec::new();
    for raw in raw_names {
        match resolve_type_filter(raw, exclude) {
            Some(TypeFilter::Type(name)) => {
                if !type_names.contains(&name) {
                    type_names.push(name);
                }
            },
            Some(TypeFilter::Glob(glob)) => globs.push(glob),
            None => {},
        }
    }
    (type_names, globs)
}

fn build_args(input: &RipgrepInput) -> Vec<String> {
    let (include_types, include_globs) = collect_type_filters(&input.include_types, false);
    let (exclude_types, exclude_globs) = collect_type_filters(&input.type_not, true);

    let mut args = vec!["--json".to_string()];
    if input.fixed_strings {
        args.push("-F".to_string());
    }
    match input.case_mode {
        Some(CaseMode::Ignore) => args.push("-i".to_string()),
        Some(CaseMode::Smart) => args.push("-S".to_string()),
        Some(CaseMode::Sensitive) | None => {},
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

/// rg `--json` text payload: plain UTF-8 or base64-encoded bytes.
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
            .ok_or_else(|| Error::message("rg JSON payload has neither 'text' nor 'bytes'"))?;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(bytes)
            .map_err(|e| Error::message(format!("invalid base64 in rg JSON output: {e}")))?;
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

/// One line of the rg `--json` stream.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "lowercase")]
enum RgMessage {
    Begin(IgnoredAny),
    Match(RgMatchData),
    Context(RgContextData),
    End(IgnoredAny),
    Summary(RgSummaryData),
}

#[derive(Debug, Serialize)]
struct SubmatchRow {
    #[serde(rename = "match")]
    matched: String,
    start: u64,
    end: u64,
}

#[derive(Debug, Serialize)]
struct MatchRow {
    path: String,
    line_number: Option<u64>,
    lines: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    submatches: Option<Vec<SubmatchRow>>,
}

#[derive(Debug, Serialize)]
struct ContextRow {
    path: String,
    line_number: Option<u64>,
    lines: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LimitsOut {
    max_matches: usize,
    max_files: usize,
    max_output_chars: usize,
    timeout_ms: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SummaryOut {
    files_with_matches: usize,
    match_count: usize,
    elapsed: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stats: Option<Value>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RipgrepResult {
    tool: &'static str,
    detail: Detail,
    found: bool,
    timed_out: bool,
    truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    truncated_reason: Option<&'static str>,
    limits: LimitsOut,
    summary: SummaryOut,
    #[serde(skip_serializing_if = "Option::is_none")]
    files: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    matches: Option<Vec<MatchRow>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    context: Option<Vec<ContextRow>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stderr: Option<String>,
    exit_code: Option<i32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    Continue,
    Stop,
}

/// Accumulated state while streaming rg's JSON output.
struct ScanState {
    detail: Detail,
    max_matches: usize,
    max_files: usize,
    max_output_chars: usize,
    stdout_chars: usize,
    seen_files: HashSet<String>,
    files: Vec<String>,
    matches: Vec<MatchRow>,
    context: Vec<ContextRow>,
    match_count: usize,
    truncated_reason: Option<&'static str>,
    stats: Option<Value>,
}

impl ScanState {
    fn new(input: &RipgrepInput) -> Self {
        Self {
            detail: input.detail,
            max_matches: input.max_matches,
            max_files: input.max_files,
            max_output_chars: input.max_output_chars,
            stdout_chars: 0,
            seen_files: HashSet::new(),
            files: Vec::new(),
            matches: Vec::new(),
            context: Vec::new(),
            match_count: 0,
            truncated_reason: None,
            stats: None,
        }
    }

    fn truncate(&mut self, reason: &'static str) -> Flow {
        self.truncated_reason = Some(reason);
        Flow::Stop
    }

    fn process_line(&mut self, line: &str) -> Result<Flow> {
        self.stdout_chars += line.len() + 1;
        if self.stdout_chars > self.max_output_chars {
            return Ok(self.truncate("maxOutputChars"));
        }
        if line.trim().is_empty() {
            return Ok(Flow::Continue);
        }
        let message: RgMessage = serde_json::from_str(line)
            .map_err(|e| Error::message(format!("rg JSON parse error: {e}")))?;
        match message {
            RgMessage::Match(data) => self.process_match(&data),
            RgMessage::Context(data) => {
                if self.detail.wants_rows() {
                    self.context.push(ContextRow {
                        path: data.path.decode()?,
                        line_number: data.line_number,
                        lines: data.lines.decode()?,
                    });
                }
                Ok(Flow::Continue)
            },
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
            if self.detail == Detail::Files {
                self.files.push(path.clone());
            }
        }
        self.match_count += 1;
        if self.detail.wants_rows() {
            let submatches = if self.detail == Detail::LinesSubmatches {
                let rows = data
                    .submatches
                    .iter()
                    .map(|sm| {
                        Ok(SubmatchRow {
                            matched: sm.matched.decode()?,
                            start: sm.start,
                            end: sm.end,
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                (!rows.is_empty()).then_some(rows)
            } else {
                None
            };
            self.matches.push(MatchRow {
                path,
                line_number: data.line_number,
                lines: data.lines.decode()?,
                submatches,
            });
        }
        if self.match_count >= self.max_matches {
            return Ok(self.truncate("maxMatches"));
        }
        Ok(Flow::Continue)
    }
}

/// Drain rg's stderr, keeping at most [`STDERR_MAX_CHARS`] characters.
///
/// Keeps reading past the cap so the child never blocks on a full pipe.
async fn collect_stderr(stderr: ChildStderr) -> std::io::Result<String> {
    let mut reader = BufReader::new(stderr);
    let mut collected = String::new();
    let mut truncated = false;
    let mut buf = [0_u8; 4096];
    loop {
        let read = reader.read(&mut buf).await?;
        if read == 0 {
            break;
        }
        if collected.len() < STDERR_MAX_CHARS {
            let chunk = String::from_utf8_lossy(&buf[..read]);
            let remaining = STDERR_MAX_CHARS - collected.len();
            if chunk.len() <= remaining {
                collected.push_str(&chunk);
            } else {
                collected.push_str(&chunk[..chunk.floor_char_boundary(remaining)]);
                truncated = true;
            }
        }
    }
    if truncated {
        collected.push('…');
    }
    Ok(collected)
}

async fn scan_stdout(stdout: ChildStdout, state: &mut ScanState) -> Result<()> {
    let mut lines = BufReader::new(stdout).lines();
    while let Some(line) = lines
        .next_line()
        .await
        .map_err(|e| Error::message(format!("failed to read rg stdout: {e}")))?
    {
        if state.process_line(&line)? == Flow::Stop {
            break;
        }
    }
    Ok(())
}

#[instrument(skip(input), fields(pattern = %input.pattern))]
async fn run_search(input: &RipgrepInput) -> Result<RipgrepResult> {
    let args = build_args(input);
    let mut command = Command::new("rg");
    command
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(cwd) = &input.cwd {
        command.current_dir(cwd);
    }

    let mut child = command
        .spawn()
        .map_err(|e| Error::message(format!("failed to start rg: {e}")))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| Error::message("rg stdout pipe missing"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| Error::message("rg stderr pipe missing"))?;

    let stderr_task = tokio::spawn(collect_stderr(stderr));

    let mut state = ScanState::new(input);
    let scan_result = tokio::time::timeout(
        Duration::from_millis(input.timeout_ms),
        scan_stdout(stdout, &mut state),
    )
    .await;

    // Kill unconditionally: a no-op when rg already exited on its own, and
    // required when scanning stopped early (limit hit, timeout, parse error).
    let _ = child.start_kill();
    let status = child
        .wait()
        .await
        .map_err(|e| Error::message(format!("failed to wait for rg: {e}")))?;
    let stderr_text = stderr_task
        .await
        .map_err(|e| Error::message(format!("rg stderr reader task failed: {e}")))?
        .map_err(|e| Error::message(format!("failed to read rg stderr: {e}")))?;

    let timed_out = scan_result.is_err();
    match scan_result {
        Ok(inner) => inner?,
        Err(_) => {
            state.truncated_reason = Some("timeout");
        },
    }
    let truncated = state.truncated_reason.is_some();

    if !truncated {
        match status.code() {
            Some(0 | 1) => {},
            Some(code) => {
                let suffix = if stderr_text.is_empty() {
                    String::new()
                } else {
                    format!(" stderr: {stderr_text}")
                };
                return Err(Error::message(format!(
                    "rg failed with exit code {code}.{suffix}"
                )));
            },
            None => {
                return Err(Error::message("rg terminated by a signal"));
            },
        }
    }

    let ScanState {
        detail,
        seen_files,
        files,
        matches,
        context,
        match_count,
        truncated_reason,
        stats,
        ..
    } = state;
    let elapsed = stats.as_ref().and_then(|s| s.get("elapsed")).cloned();
    let wants_rows = detail.wants_rows();

    Ok(RipgrepResult {
        tool: "ripgrep",
        detail,
        found: match_count > 0,
        timed_out,
        truncated,
        truncated_reason,
        limits: LimitsOut {
            max_matches: input.max_matches,
            max_files: input.max_files,
            max_output_chars: input.max_output_chars,
            timeout_ms: input.timeout_ms,
        },
        summary: SummaryOut {
            files_with_matches: seen_files.len(),
            match_count,
            elapsed,
            stats,
        },
        files: (detail == Detail::Files).then_some(files),
        matches: wants_rows.then_some(matches),
        context: wants_rows.then_some(context),
        stderr: (!stderr_text.is_empty()).then_some(stderr_text),
        exit_code: status.code(),
    })
}

async fn run_tool(params: Value) -> Result<Value> {
    let input: RipgrepInput = serde_json::from_value(without_null_params(params))
        .map_err(|e| Error::message(format!("invalid ripgrep parameters: {e}")))?;
    validate_input(&input)?;
    let result = run_search(&input).await?;
    Ok(serde_json::to_value(result)?)
}

#[async_trait]
impl AgentTool for RipgrepTool {
    fn name(&self) -> &str {
        "ripgrep"
    }

    fn description(&self) -> &str {
        "Search files using ripgrep (rg) with structured JSON output."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["pattern"],
            "properties": {
                "pattern": {
                    "type": "string",
                    "minLength": 1,
                    "description": "Pattern to search for."
                },
                "paths": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Paths to search (defaults to the working directory)."
                },
                "cwd": {
                    "type": "string",
                    "description": "Working directory for the rg process."
                },
                "fixedStrings": {
                    "type": "boolean",
                    "default": false,
                    "description": "Use fixed strings (-F)."
                },
                "caseMode": {
                    "type": "string",
                    "enum": ["sensitive", "ignore", "smart"],
                    "description": "Case matching mode."
                },
                "detail": {
                    "type": "string",
                    "enum": ["summary", "files", "lines", "lines+submatches"],
                    "default": "lines",
                    "description": "Detail level for results."
                },
                "glob": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Glob patterns mapped to --glob."
                },
                "type": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Ripgrep file type names from rg --type-list. Common extension-like values such as tsx/jsx are normalized; unknown extension-like values are converted to glob filters."
                },
                "typeNot": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Ripgrep file type names to exclude via --type-not. Common extension-like values such as tsx/jsx are normalized; unknown extension-like values are converted to exclusion glob filters."
                },
                "contextLines": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Context lines mapped to -C."
                },
                "maxMatches": {
                    "type": "integer",
                    "minimum": 1,
                    "default": DEFAULT_MAX_MATCHES,
                    "description": "Maximum number of match records to return."
                },
                "maxFiles": {
                    "type": "integer",
                    "minimum": 1,
                    "default": DEFAULT_MAX_FILES,
                    "description": "Maximum number of files with matches to include."
                },
                "maxOutputChars": {
                    "type": "integer",
                    "minimum": 1,
                    "default": DEFAULT_MAX_OUTPUT_CHARS,
                    "description": "Maximum rg stdout characters to process."
                },
                "timeoutMs": {
                    "type": "integer",
                    "minimum": 0,
                    "default": DEFAULT_TIMEOUT_MS,
                    "description": "Timeout in milliseconds for the search."
                },
                "includeHidden": {
                    "type": "boolean",
                    "default": true,
                    "description": "Include hidden files (maps to --hidden)."
                },
                "unrestricted": {
                    "type": "integer",
                    "enum": [0, 1, 2, 3],
                    "default": 3,
                    "description": "Ignore rules level (maps to -u/-uu/-uuu)."
                },
                "followSymlinks": {
                    "type": "boolean",
                    "default": false,
                    "description": "Follow symlinks (maps to --follow)."
                }
            }
        })
    }

    async fn execute(&self, params: Value) -> anyhow::Result<Value> {
        let result = run_tool(params).await;
        #[cfg(feature = "metrics")]
        match &result {
            Ok(_) => {
                counter!(
                    tools_metrics::EXECUTIONS_TOTAL,
                    labels::TOOL => "ripgrep".to_string(),
                    labels::SUCCESS => "true".to_string()
                )
                .increment(1);
            },
            Err(_) => {
                counter!(
                    tools_metrics::EXECUTION_ERRORS_TOTAL,
                    labels::TOOL => "ripgrep".to_string()
                )
                .increment(1);
            },
        }
        Ok(result?)
    }
}

#[allow(clippy::unwrap_used, clippy::expect_used)]
#[cfg(test)]
mod tests {
    use super::*;

    fn input_from(params: Value) -> RipgrepInput {
        serde_json::from_value(params).unwrap()
    }

    #[test]
    fn build_args_defaults() {
        let input = input_from(json!({ "pattern": "needle" }));
        assert_eq!(build_args(&input), vec![
            "--json", "--hidden", "-uuu", "--", "needle"
        ]);
    }

    #[test]
    fn build_args_full_flags() {
        let input = input_from(json!({
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
        assert_eq!(build_args(&input), vec![
            "--json", "-F", "-i", "-u", "--follow", "-C", "2", "--glob", "*.rs", "--", "needle",
            "src", "docs"
        ]);
    }

    #[test]
    fn build_args_smart_case_and_no_unrestricted() {
        let input = input_from(json!({
            "pattern": "needle",
            "caseMode": "smart",
            "unrestricted": 0
        }));
        assert_eq!(build_args(&input), vec![
            "--json", "-S", "--hidden", "--", "needle"
        ]);
    }

    #[test]
    fn build_args_type_filters() {
        let input = input_from(json!({
            "pattern": "needle",
            "type": ["tsx", "rust", "customext", "ts"],
            "typeNot": ["jsx", "otherext"]
        }));
        assert_eq!(build_args(&input), vec![
            "--json",
            "--hidden",
            "-uuu",
            "--glob",
            "*.customext",
            "--glob",
            "!*.otherext",
            "--type",
            "ts",
            "--type",
            "rust",
            "--type-not",
            "js",
            "--",
            "needle"
        ]);
    }

    #[test]
    fn resolve_type_filter_variants() {
        assert_eq!(
            resolve_type_filter("tsx", false),
            Some(TypeFilter::Type("ts".to_string()))
        );
        assert_eq!(
            resolve_type_filter(".RUST", false),
            Some(TypeFilter::Type("rust".to_string()))
        );
        assert_eq!(
            resolve_type_filter("myext", false),
            Some(TypeFilter::Glob("*.myext".to_string()))
        );
        assert_eq!(
            resolve_type_filter("myext", true),
            Some(TypeFilter::Glob("!*.myext".to_string()))
        );
        assert_eq!(
            resolve_type_filter("not a type", false),
            Some(TypeFilter::Type("not a type".to_string()))
        );
        assert_eq!(resolve_type_filter("  ", false), None);
        assert_eq!(resolve_type_filter(".", false), None);
    }

    #[test]
    fn is_extension_like_cases() {
        assert!(is_extension_like("ts"));
        assert!(is_extension_like(".tsx"));
        assert!(is_extension_like("my-ext_2"));
        assert!(!is_extension_like("-bad"));
        assert!(!is_extension_like("a.b"));
        assert!(!is_extension_like(""));
    }

    #[test]
    fn rg_text_decodes_text_and_bytes() {
        let text = RgText {
            text: Some("hello".to_string()),
            bytes: None,
        };
        assert_eq!(text.decode().unwrap(), "hello");

        let bytes = RgText {
            text: None,
            bytes: Some(base64::engine::general_purpose::STANDARD.encode("world")),
        };
        assert_eq!(bytes.decode().unwrap(), "world");

        let invalid = RgText {
            text: None,
            bytes: Some("!!!".to_string()),
        };
        assert!(invalid.decode().unwrap_err().to_string().contains("base64"));

        let empty = RgText {
            text: None,
            bytes: None,
        };
        assert!(
            empty
                .decode()
                .unwrap_err()
                .to_string()
                .contains("neither 'text' nor 'bytes'")
        );
    }

    #[test]
    fn parses_rg_json_messages() {
        let matched: RgMessage = serde_json::from_str(
            r#"{"type":"match","data":{"path":{"text":"a.rs"},"lines":{"text":"fn main() {}\n"},"line_number":3,"absolute_offset":10,"submatches":[{"match":{"text":"main"},"start":3,"end":7}]}}"#,
        )
        .unwrap();
        let RgMessage::Match(data) = matched else {
            panic!("expected match message");
        };
        assert_eq!(data.path.decode().unwrap(), "a.rs");
        assert_eq!(data.line_number, Some(3));
        assert_eq!(data.submatches.len(), 1);
        assert_eq!(data.submatches[0].matched.decode().unwrap(), "main");

        let context: RgMessage = serde_json::from_str(
            r#"{"type":"context","data":{"path":{"text":"a.rs"},"lines":{"text":"// hi\n"},"line_number":2,"absolute_offset":4,"submatches":[]}}"#,
        )
        .unwrap();
        assert!(matches!(context, RgMessage::Context(_)));

        let summary: RgMessage = serde_json::from_str(
            r#"{"type":"summary","data":{"elapsed_total":{"secs":0,"nanos":100,"human":"0.0s"},"stats":{"matches":1,"elapsed":{"secs":0,"nanos":50,"human":"0.0s"}}}}"#,
        )
        .unwrap();
        let RgMessage::Summary(data) = summary else {
            panic!("expected summary message");
        };
        assert_eq!(data.stats.unwrap()["matches"], 1);
    }

    fn match_line(path: &str, line_number: u64) -> String {
        json!({
            "type": "match",
            "data": {
                "path": { "text": path },
                "lines": { "text": "match line\n" },
                "line_number": line_number,
                "absolute_offset": 0,
                "submatches": [{ "match": { "text": "match" }, "start": 0, "end": 5 }]
            }
        })
        .to_string()
    }

    #[test]
    fn scan_state_enforces_max_matches() {
        let input = input_from(json!({ "pattern": "x", "maxMatches": 2 }));
        let mut state = ScanState::new(&input);
        assert_eq!(
            state.process_line(&match_line("a.rs", 1)).unwrap(),
            Flow::Continue
        );
        assert_eq!(
            state.process_line(&match_line("a.rs", 2)).unwrap(),
            Flow::Stop
        );
        assert_eq!(state.truncated_reason, Some("maxMatches"));
        assert_eq!(state.match_count, 2);
        assert_eq!(state.matches.len(), 2);
    }

    #[test]
    fn scan_state_enforces_max_files() {
        let input = input_from(json!({ "pattern": "x", "maxFiles": 1 }));
        let mut state = ScanState::new(&input);
        assert_eq!(
            state.process_line(&match_line("a.rs", 1)).unwrap(),
            Flow::Continue
        );
        assert_eq!(
            state.process_line(&match_line("b.rs", 1)).unwrap(),
            Flow::Stop
        );
        assert_eq!(state.truncated_reason, Some("maxFiles"));
        assert_eq!(state.seen_files.len(), 1);
        assert_eq!(state.match_count, 1);
    }

    #[test]
    fn scan_state_enforces_max_output_chars() {
        let input = input_from(json!({ "pattern": "x", "maxOutputChars": 10 }));
        let mut state = ScanState::new(&input);
        assert_eq!(
            state.process_line(&match_line("a.rs", 1)).unwrap(),
            Flow::Stop
        );
        assert_eq!(state.truncated_reason, Some("maxOutputChars"));
        assert_eq!(state.match_count, 0);
    }

    #[test]
    fn scan_state_rejects_malformed_json() {
        let input = input_from(json!({ "pattern": "x" }));
        let mut state = ScanState::new(&input);
        let err = state.process_line("{not json").unwrap_err();
        assert!(err.to_string().contains("rg JSON parse error"));
    }

    #[test]
    fn scan_state_skips_submatches_in_lines_mode() {
        let input = input_from(json!({ "pattern": "x", "detail": "lines" }));
        let mut state = ScanState::new(&input);
        state.process_line(&match_line("a.rs", 1)).unwrap();
        assert!(state.matches[0].submatches.is_none());

        let input = input_from(json!({ "pattern": "x", "detail": "lines+submatches" }));
        let mut state = ScanState::new(&input);
        state.process_line(&match_line("a.rs", 1)).unwrap();
        let submatches = state.matches[0].submatches.as_ref().unwrap();
        assert_eq!(submatches[0].matched, "match");
        assert_eq!(submatches[0].start, 0);
        assert_eq!(submatches[0].end, 5);
    }

    #[test]
    fn validate_input_rejects_bad_values() {
        let empty_pattern = input_from(json!({ "pattern": "" }));
        assert!(validate_input(&empty_pattern).is_err());

        let zero_matches = input_from(json!({ "pattern": "x", "maxMatches": 0 }));
        assert!(validate_input(&zero_matches).is_err());

        let zero_files = input_from(json!({ "pattern": "x", "maxFiles": 0 }));
        assert!(validate_input(&zero_files).is_err());

        let zero_output = input_from(json!({ "pattern": "x", "maxOutputChars": 0 }));
        assert!(validate_input(&zero_output).is_err());

        let bad_unrestricted = input_from(json!({ "pattern": "x", "unrestricted": 4 }));
        assert!(validate_input(&bad_unrestricted).is_err());

        let ok = input_from(json!({ "pattern": "x" }));
        assert!(validate_input(&ok).is_ok());
    }

    #[test]
    fn input_tolerates_session_key_and_nulls() {
        let input = input_from(without_null_params(json!({
            "pattern": "x",
            "_session_key": "session:test",
            "cwd": null,
            "paths": null
        })));
        assert_eq!(input.pattern, "x");
        assert!(input.cwd.is_none());
        assert!(input.paths.is_empty());
    }

    async fn setup_tree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        tokio::fs::write(
            dir.path().join("alpha.txt"),
            "first line\nripgrep-needle here\nlast line\n",
        )
        .await
        .unwrap();
        tokio::fs::write(
            dir.path().join("beta.txt"),
            "ripgrep-needle one\nripgrep-needle two\n",
        )
        .await
        .unwrap();
        tokio::fs::write(dir.path().join("gamma.rs"), "fn empty() {}\n")
            .await
            .unwrap();
        dir
    }

    #[tokio::test]
    async fn execute_finds_matches_with_lines_detail() {
        let dir = setup_tree().await;
        let tool = RipgrepTool::new();
        let value = tool
            .execute(json!({
                "pattern": "ripgrep-needle",
                "fixedStrings": true,
                "cwd": dir.path().to_str().unwrap()
            }))
            .await
            .unwrap();

        assert_eq!(value["tool"], "ripgrep");
        assert_eq!(value["detail"], "lines");
        assert_eq!(value["found"], true);
        assert_eq!(value["timedOut"], false);
        assert_eq!(value["truncated"], false);
        assert_eq!(value["exitCode"], 0);
        assert_eq!(value["summary"]["matchCount"], 3);
        assert_eq!(value["summary"]["filesWithMatches"], 2);
        assert_eq!(value["matches"].as_array().unwrap().len(), 3);
        assert!(value["matches"][0]["submatches"].is_null());
        assert!(value.get("files").is_none());
    }

    #[tokio::test]
    async fn execute_files_detail_lists_paths() {
        let dir = setup_tree().await;
        let tool = RipgrepTool::new();
        let value = tool
            .execute(json!({
                "pattern": "ripgrep-needle",
                "fixedStrings": true,
                "detail": "files",
                "cwd": dir.path().to_str().unwrap()
            }))
            .await
            .unwrap();

        let files = value["files"].as_array().unwrap();
        assert_eq!(files.len(), 2);
        assert!(value.get("matches").is_none());
        assert!(value.get("context").is_none());
    }

    #[tokio::test]
    async fn execute_summary_detail_omits_rows() {
        let dir = setup_tree().await;
        let tool = RipgrepTool::new();
        let value = tool
            .execute(json!({
                "pattern": "ripgrep-needle",
                "fixedStrings": true,
                "detail": "summary",
                "cwd": dir.path().to_str().unwrap()
            }))
            .await
            .unwrap();

        assert_eq!(value["found"], true);
        assert_eq!(value["summary"]["matchCount"], 3);
        assert!(value.get("files").is_none());
        assert!(value.get("matches").is_none());
        assert!(value.get("context").is_none());
    }

    #[tokio::test]
    async fn execute_reports_no_match() {
        let dir = setup_tree().await;
        let tool = RipgrepTool::new();
        let value = tool
            .execute(json!({
                "pattern": "definitely-not-present-anywhere",
                "fixedStrings": true,
                "cwd": dir.path().to_str().unwrap()
            }))
            .await
            .unwrap();

        assert_eq!(value["found"], false);
        assert_eq!(value["exitCode"], 1);
        assert_eq!(value["summary"]["matchCount"], 0);
    }

    #[tokio::test]
    async fn execute_truncates_on_max_matches() {
        let dir = setup_tree().await;
        let tool = RipgrepTool::new();
        let value = tool
            .execute(json!({
                "pattern": "ripgrep-needle",
                "fixedStrings": true,
                "maxMatches": 1,
                "cwd": dir.path().to_str().unwrap()
            }))
            .await
            .unwrap();

        assert_eq!(value["truncated"], true);
        assert_eq!(value["truncatedReason"], "maxMatches");
        assert_eq!(value["summary"]["matchCount"], 1);
    }

    #[tokio::test]
    async fn execute_returns_context_and_submatches() {
        let dir = setup_tree().await;
        let tool = RipgrepTool::new();
        let value = tool
            .execute(json!({
                "pattern": "ripgrep-needle here",
                "fixedStrings": true,
                "detail": "lines+submatches",
                "contextLines": 1,
                "paths": [dir.path().join("alpha.txt").to_str().unwrap()]
            }))
            .await
            .unwrap();

        let matches = value["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0]["line_number"], 2);
        assert_eq!(matches[0]["submatches"][0]["match"], "ripgrep-needle here");
        let context = value["context"].as_array().unwrap();
        assert_eq!(context.len(), 2);
    }

    #[tokio::test]
    async fn execute_glob_filters_files() {
        let dir = setup_tree().await;
        let tool = RipgrepTool::new();
        let value = tool
            .execute(json!({
                "pattern": "ripgrep-needle",
                "fixedStrings": true,
                "detail": "files",
                "glob": ["alpha.*"],
                "cwd": dir.path().to_str().unwrap()
            }))
            .await
            .unwrap();

        let files = value["files"].as_array().unwrap();
        assert_eq!(files.len(), 1);
        assert!(files[0].as_str().unwrap().contains("alpha.txt"));
    }

    #[tokio::test]
    async fn execute_fails_on_invalid_regex() {
        let dir = setup_tree().await;
        let tool = RipgrepTool::new();
        let err = tool
            .execute(json!({
                "pattern": "(unbalanced",
                "cwd": dir.path().to_str().unwrap()
            }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("exit code 2"));
    }

    #[tokio::test]
    async fn execute_fails_on_missing_cwd() {
        let tool = RipgrepTool::new();
        let err = tool
            .execute(json!({
                "pattern": "x",
                "cwd": "/definitely/not/a/real/ripgrep/cwd"
            }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("failed to start rg"));
    }

    #[tokio::test]
    async fn execute_rejects_missing_pattern() {
        let tool = RipgrepTool::new();
        let err = tool.execute(json!({})).await.unwrap_err();
        assert!(err.to_string().contains("invalid ripgrep parameters"));
    }
}
