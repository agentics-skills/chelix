//! Single reader and writer for skill markdown files.
//!
//! Requests enqueue one job on an unbounded channel. One thread receives
//! those jobs in order and runs the file work. The thread exits when the
//! sender is dropped.

use std::{
    fs::File,
    io::{Read, Write},
    path::Path,
};

use {serde::Deserialize, std::sync::Arc};

use {
    chelix_call_bus::CallBus,
    chelix_service_traits::{
        CreateSkillFile, PatchSkillFile, ReadSkillFile, ReplaceSkillFile, SkillFileError,
        SkillFileRead, WriteNewSkillFile,
    },
};

use crate::{
    error::{Error, Result},
    parse::{resolve_name_or_slug, split_frontmatter_raw},
    types::{SkillContent, SkillMetadata, SkillOrigin},
};

type Job = Box<dyn FnOnce() + Send>;

/// In-process queue for skill file reads and writes.
pub struct SkillFiles {
    tx: tokio::sync::mpsc::UnboundedSender<Job>,
}

impl SkillFiles {
    /// Spawn the single worker thread that runs queued file operations.
    pub fn new() -> std::result::Result<Arc<Self>, String> {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Job>();
        std::thread::Builder::new()
            .name("skill-file".into())
            .spawn(move || {
                while let Some(job) = rx.blocking_recv() {
                    job();
                }
            })
            .map_err(|error| error.to_string())?;
        Ok(Arc::new(Self { tx }))
    }
}

const KNOWN_KEYS: &[&str] = &[
    "name",
    "description",
    "slug",
    "display_name",
    "homepage",
    "license",
    "compatibility",
    "origin",
    "denied_agents",
    "allowed_agents",
];

/// Fields a create or replace writes into the known frontmatter.
pub struct SkillFields {
    pub name: String,
    pub description: String,
    pub body: String,
}

/// Read a skill file and return the interpreted document.
pub async fn read(
    files: &SkillFiles,
    path: &Path,
    max_bytes: Option<u64>,
    mode: SkillFileRead,
) -> Result<SkillContent> {
    let path = path.to_path_buf();
    with_file(files, move || {
        let bytes = read_capped(&path, max_bytes)?;
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| Error::Parse("skill file is not valid UTF-8".into()))?;
        interpret_mode(text, &path, mode)
    })
    .await
}

/// Interpret skill markdown already loaded by the caller.
pub fn interpret(content: &str, path: &Path) -> Result<SkillContent> {
    interpret_mode(content, path, SkillFileRead::Detect)
}

fn interpret_mode(content: &str, path: &Path, mode: SkillFileRead) -> Result<SkillContent> {
    Ok(load(content, path, mode)?.content)
}

/// Create a skill file. An existing file is left unchanged.
pub async fn create(
    files: &SkillFiles,
    path: &Path,
    fields: SkillFields,
    creator_agent_id: Option<&str>,
) -> Result<SkillContent> {
    let path = path.to_path_buf();
    let creator = creator_agent_id.map(str::to_string);
    with_file(files, move || match File::open(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let allowed_agents = creator.into_iter().collect();
            let raw = RawKnown {
                name: fields.name,
                description: fields.description,
                allowed_agents,
                ..RawKnown::default()
            };
            let body = fields.body;
            let rendered = render("", &raw, None, "", &body)?;
            write_file(&path, &rendered)?;
            interpret(&rendered, &path)
        },
        Err(error) => Err(error.into()),
        Ok(_) => Err(Error::AlreadyExists(path.display().to_string())),
    })
    .await
}

/// Write a new skill document. An existing file is left unchanged.
pub async fn write_new(files: &SkillFiles, path: &Path, content: String) -> Result<SkillContent> {
    let path = path.to_path_buf();
    with_file(files, move || match File::open(&path) {
        Ok(_) => Err(Error::AlreadyExists(path.display().to_string())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let loaded = load(&content, &path, SkillFileRead::Detect)?;
            let rendered = render(
                &loaded.preamble,
                &loaded.raw,
                loaded.origin_text.as_deref(),
                &loaded.tail,
                &loaded.content.body,
            )?;
            write_file(&path, &rendered)?;
            interpret(&rendered, &path)
        },
        Err(error) => Err(error.into()),
    })
    .await
}

/// Replace name, description, and body. Missing files are created with empty agent lists.
pub async fn replace(files: &SkillFiles, path: &Path, fields: SkillFields) -> Result<SkillContent> {
    let path = path.to_path_buf();
    with_file(files, move || {
        let loaded = match read_capped(&path, None) {
            Ok(bytes) => {
                let text = std::str::from_utf8(&bytes)
                    .map_err(|_| Error::Parse("skill file is not valid UTF-8".into()))?;
                Some(load(text, &path, SkillFileRead::Detect)?)
            },
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        let rendered = match loaded {
            None => {
                let raw = RawKnown {
                    name: fields.name,
                    description: fields.description,
                    ..RawKnown::default()
                };
                render("", &raw, None, "", &fields.body)?
            },
            Some(loaded) => {
                let mut raw = loaded.raw;
                raw.name = fields.name;
                raw.description = fields.description;
                render(
                    &loaded.preamble,
                    &raw,
                    loaded.origin_text.as_deref(),
                    &loaded.tail,
                    &fields.body,
                )?
            },
        };
        write_file(&path, &rendered)?;
        interpret(&rendered, &path)
    })
    .await
}

/// Apply ordered body replacements. The file must already exist.
pub async fn patch(
    files: &SkillFiles,
    path: &Path,
    patches: &[(String, String)],
    description: Option<&str>,
) -> Result<SkillContent> {
    let path = path.to_path_buf();
    let patches: Vec<(String, String)> = patches.to_vec();
    let description = description.map(str::to_string);
    with_file(files, move || {
        let bytes = match read_capped(&path, None) {
            Ok(bytes) => bytes,
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(Error::NotFound(format!(
                    "skill file not found: {}",
                    path.display()
                )));
            },
            Err(error) => return Err(error),
        };
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| Error::Parse("skill file is not valid UTF-8".into()))?;
        let loaded = load(text, &path, SkillFileRead::Detect)?;
        let mut body = loaded.content.body;
        for (index, (find, replace_with)) in patches.iter().enumerate() {
            if find.is_empty() {
                return Err(Error::Validation(format!(
                    "patch[{index}]: 'find' must not be empty"
                )));
            }
            if !body.contains(find) {
                return Err(Error::Validation(format!(
                    "patch[{index}]: string not found in skill body: {find:?}"
                )));
            }
            body = body.replacen(find, replace_with, 1);
        }
        let mut raw = loaded.raw;
        if let Some(description) = description {
            raw.description = description;
        }
        let rendered = render(
            &loaded.preamble,
            &raw,
            loaded.origin_text.as_deref(),
            &loaded.tail,
            &body,
        )?;
        write_file(&path, &rendered)?;
        interpret(&rendered, &path)
    })
    .await
}

async fn with_file<T, F>(files: &SkillFiles, op: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    let (tx, rx) = tokio::sync::oneshot::channel();
    files
        .tx
        .send(Box::new(move || {
            let _ = tx.send(op());
        }))
        .map_err(|_| Error::Validation("skill file queue closed".into()))?;
    rx.await
        .map_err(|_| Error::Validation("skill file queue closed".into()))?
}

fn read_capped(path: &Path, max_bytes: Option<u64>) -> Result<Vec<u8>> {
    let mut file = File::open(path)?;
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let read = match file.read(&mut chunk) {
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        };
        if read == 0 {
            break;
        }
        if let Some(max) = max_bytes
            && (bytes.len() + read) as u64 > max
        {
            return Err(Error::Validation(format!(
                "skill file exceeds maximum size of {max} bytes"
            )));
        }
        bytes.extend_from_slice(&chunk[..read]);
    }
    Ok(bytes)
}

fn write_file(path: &Path, contents: &str) -> Result<()> {
    let mut file = File::create(path)?;
    file.write_all(contents.as_bytes())?;
    Ok(())
}

#[derive(Default)]
struct RawKnown {
    name: String,
    description: String,
    slug: Option<String>,
    display_name: Option<String>,
    homepage: Option<String>,
    license: Option<String>,
    compatibility: Option<String>,
    allowed_agents: Vec<String>,
    denied_agents: Vec<String>,
}

struct Loaded {
    content: SkillContent,
    preamble: String,
    tail: String,
    origin_text: Option<String>,
    raw: RawKnown,
}

fn load(content: &str, path: &Path, mode: SkillFileRead) -> Result<Loaded> {
    let skill_md = match mode {
        SkillFileRead::SkillDocument => true,
        SkillFileRead::Detect | SkillFileRead::Catalog => is_skill_md(path),
    };
    let metadata_path = if skill_md {
        path.parent().unwrap_or(path).to_path_buf()
    } else {
        path.to_path_buf()
    };
    if !content.trim_start().starts_with("---") {
        if skill_md {
            return Err(Error::Parse(
                "SKILL.md must start with YAML frontmatter delimited by ---".into(),
            ));
        }
        return Ok(loose_document(content, metadata_path));
    }
    let (frontmatter, raw_body) = match split_frontmatter_raw(content) {
        Ok(parts) => parts,
        Err(_) if !skill_md => return Ok(loose_document(content, metadata_path)),
        Err(error) => return Err(error),
    };
    let body = raw_body
        .strip_prefix("\r\n")
        .or_else(|| raw_body.strip_prefix('\n'))
        .unwrap_or(raw_body)
        .to_string();
    let frontmatter = strip_one_leading_newline(frontmatter);
    let raw_meta: RawMeta = match serde_yaml::from_str(frontmatter) {
        Ok(meta) => meta,
        Err(_) if matches!(mode, SkillFileRead::Catalog) && !skill_md => {
            return Ok(loose_document(content, metadata_path));
        },
        Err(error) => {
            return Err(Error::Parse(format!(
                "invalid SKILL.md frontmatter: {error}"
            )));
        },
    };
    let (preamble, tail, origin_text) = split_preserved(frontmatter);
    let raw = RawKnown {
        name: raw_meta.name.clone(),
        description: raw_meta.description.clone(),
        slug: raw_meta.slug.clone(),
        display_name: raw_meta.display_name.clone(),
        homepage: raw_meta.homepage.clone(),
        license: raw_meta.license.clone(),
        compatibility: raw_meta.compatibility.clone(),
        allowed_agents: raw_meta.allowed_agents.clone(),
        denied_agents: raw_meta.denied_agents.clone(),
    };
    let mut metadata = SkillMetadata {
        name: raw_meta.name,
        slug: raw_meta.slug,
        display_name: raw_meta.display_name,
        description: raw_meta.description,
        homepage: raw_meta.homepage,
        license: raw_meta.license,
        compatibility: raw_meta.compatibility,
        allowed_agents: raw_meta.allowed_agents,
        denied_agents: raw_meta.denied_agents,
        origin: raw_meta.origin,
        path: metadata_path,
        ..SkillMetadata::default()
    };
    if skill_md {
        resolve_name_or_slug(&mut metadata)?;
    }
    Ok(Loaded {
        content: SkillContent { metadata, body },
        preamble,
        tail,
        origin_text,
        raw,
    })
}

fn loose_document(content: &str, path: std::path::PathBuf) -> Loaded {
    Loaded {
        content: SkillContent {
            metadata: SkillMetadata {
                path,
                ..SkillMetadata::default()
            },
            body: content.to_string(),
        },
        preamble: String::new(),
        tail: String::new(),
        origin_text: None,
        raw: RawKnown::default(),
    }
}

fn is_skill_md(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == "SKILL.md")
}

fn strip_one_leading_newline(text: &str) -> &str {
    text.strip_prefix("\r\n")
        .or_else(|| text.strip_prefix('\n'))
        .unwrap_or(text)
}

fn split_preserved(frontmatter: &str) -> (String, String, Option<String>) {
    let mut preamble = String::new();
    let mut tail = String::new();
    let mut origin_text = None;
    let mut seen_field = false;
    let mut rest = frontmatter;
    while !rest.is_empty() {
        let (piece, next) = next_piece(rest);
        rest = next;
        if is_key_piece(&piece) {
            seen_field = true;
            let key = key_name(&piece);
            if key == "origin" {
                origin_text = Some(piece);
            } else if !KNOWN_KEYS.contains(&key) {
                tail.push_str(&piece);
            }
        } else if seen_field {
            tail.push_str(&piece);
        } else {
            preamble.push_str(&piece);
        }
    }
    (preamble, tail, origin_text)
}

fn next_piece(text: &str) -> (String, &str) {
    let lines = line_spans(text);
    if lines.is_empty() {
        return (text.to_string(), "");
    }
    let first = lines[0];
    if !is_key_line(first) {
        return (first.to_string(), &text[first.len()..]);
    }
    let mut end = first.len();
    for line in lines.iter().skip(1) {
        if is_key_line(line) {
            break;
        }
        if continues_block(line, &text[end + line.len()..]) {
            end += line.len();
        } else {
            break;
        }
    }
    (text[..end].to_string(), &text[end..])
}

fn line_spans(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    for (index, byte) in text.bytes().enumerate() {
        if byte == b'\n' {
            lines.push(&text[start..=index]);
            start = index + 1;
        }
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

fn is_key_piece(piece: &str) -> bool {
    line_spans(piece)
        .first()
        .is_some_and(|line| is_key_line(line))
}

fn is_key_line(line: &str) -> bool {
    let core = line.trim_end_matches(['\n', '\r']);
    !core.is_empty() && !core.starts_with([' ', '\t', '#', '-']) && core.contains(':')
}

fn continues_block(line: &str, rest: &str) -> bool {
    let core = line.trim_end_matches(['\n', '\r']);
    if core.starts_with([' ', '\t', '-']) {
        return true;
    }
    if !(core.is_empty() || core.starts_with('#')) {
        return false;
    }
    let mut rest = rest;
    loop {
        if rest.is_empty() {
            return false;
        }
        let (next_line, after) = split_first_line(rest);
        let next_core = next_line.trim_end_matches(['\n', '\r']);
        if next_core.is_empty() || next_core.starts_with('#') {
            rest = after;
            continue;
        }
        return next_core.starts_with([' ', '\t', '-']);
    }
}

fn split_first_line(text: &str) -> (&str, &str) {
    match text.find('\n') {
        Some(index) => (&text[..=index], &text[index + 1..]),
        None => (text, ""),
    }
}

fn key_name(piece: &str) -> &str {
    piece
        .lines()
        .next()
        .and_then(|line| line.split_once(':'))
        .map(|(key, _)| key.trim())
        .unwrap_or("")
}

fn render(
    preamble: &str,
    raw: &RawKnown,
    origin_text: Option<&str>,
    tail: &str,
    body: &str,
) -> Result<String> {
    let mut frontmatter = String::new();
    frontmatter.push_str(preamble);
    push_field(&mut frontmatter, "name", &raw.name)?;
    push_field(&mut frontmatter, "description", &raw.description)?;
    if let Some(slug) = &raw.slug {
        push_field(&mut frontmatter, "slug", slug)?;
    }
    if let Some(display_name) = &raw.display_name {
        push_field(&mut frontmatter, "display_name", display_name)?;
    }
    if let Some(homepage) = &raw.homepage {
        push_field(&mut frontmatter, "homepage", homepage)?;
    }
    if let Some(license) = &raw.license {
        push_field(&mut frontmatter, "license", license)?;
    }
    if let Some(compatibility) = &raw.compatibility {
        push_field(&mut frontmatter, "compatibility", compatibility)?;
    }
    if let Some(origin) = origin_text {
        frontmatter.push_str(origin);
        if !origin.ends_with('\n') {
            frontmatter.push('\n');
        }
    }
    frontmatter.push_str(&agent_block("denied_agents", &raw.denied_agents)?);
    frontmatter.push_str(&agent_block("allowed_agents", &raw.allowed_agents)?);
    frontmatter.push_str(tail);
    if !frontmatter.ends_with('\n') {
        frontmatter.push('\n');
    }
    Ok(format!("---\n{frontmatter}---\n{body}"))
}

fn push_field(out: &mut String, key: &str, value: &str) -> Result<()> {
    let rendered = serde_yaml::to_string(value)?;
    let scalar = rendered
        .trim_start_matches("---")
        .trim_matches(|ch: char| ch == '\n' || ch == '\r');
    out.push_str(key);
    out.push_str(": ");
    out.push_str(scalar);
    out.push('\n');
    Ok(())
}

fn agent_block(key: &str, ids: &[String]) -> Result<String> {
    if ids.is_empty() {
        return Ok(format!("{key}:\n"));
    }
    let mut out = format!("{key}:\n");
    for id in ids {
        let rendered = serde_yaml::to_string(id)?;
        let scalar = rendered
            .trim_start_matches("---")
            .trim_matches(|ch: char| ch == '\n' || ch == '\r');
        out.push_str("- ");
        out.push_str(scalar);
        out.push('\n');
    }
    Ok(out)
}

#[derive(Debug, Deserialize)]
struct RawMeta {
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    slug: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    homepage: Option<String>,
    #[serde(default)]
    license: Option<String>,
    #[serde(default)]
    compatibility: Option<String>,
    #[serde(default)]
    origin: Option<SkillOrigin>,
    #[serde(default, deserialize_with = "crate::types::deserialize_agent_list")]
    allowed_agents: Vec<String>,
    #[serde(default, deserialize_with = "crate::types::deserialize_agent_list")]
    denied_agents: Vec<String>,
}

fn bus_error(error: Error) -> SkillFileError {
    match error {
        Error::NotFound(message) => SkillFileError::NotFound(message),
        Error::AlreadyExists(message) => SkillFileError::AlreadyExists(message),
        Error::Io(io) if io.kind() == std::io::ErrorKind::NotFound => {
            SkillFileError::NotFound(io.to_string())
        },
        other => SkillFileError::Failed(other.to_string()),
    }
}

pub(crate) fn from_call(error: chelix_call_bus::CallError<SkillFileError>) -> Error {
    match error {
        chelix_call_bus::CallError::Failed(SkillFileError::NotFound(message)) => {
            Error::NotFound(message)
        },
        chelix_call_bus::CallError::Failed(SkillFileError::AlreadyExists(message)) => {
            Error::AlreadyExists(message)
        },
        chelix_call_bus::CallError::Failed(SkillFileError::Failed(message)) => {
            Error::Parse(message)
        },
        other => Error::Validation(other.to_string()),
    }
}

/// Register the skill file procedures on a process bus.
pub fn register_skill_files(
    bus: &CallBus,
    files: &Arc<SkillFiles>,
) -> std::result::Result<(), chelix_call_bus::RegError> {
    let read_files = Arc::clone(files);
    bus.register(move |request: ReadSkillFile| {
        let files = Arc::clone(&read_files);
        async move {
            read(&files, &request.path, request.max_bytes, request.mode)
                .await
                .map_err(bus_error)
        }
    })?;
    let create_files = Arc::clone(files);
    bus.register(move |request: CreateSkillFile| {
        let files = Arc::clone(&create_files);
        async move {
            create(
                &files,
                &request.path,
                SkillFields {
                    name: request.name,
                    description: request.description,
                    body: request.body,
                },
                request.creator_agent_id.as_deref(),
            )
            .await
            .map_err(bus_error)
        }
    })?;
    let replace_files = Arc::clone(files);
    bus.register(move |request: ReplaceSkillFile| {
        let files = Arc::clone(&replace_files);
        async move {
            replace(&files, &request.path, SkillFields {
                name: request.name,
                description: request.description,
                body: request.body,
            })
            .await
            .map_err(bus_error)
        }
    })?;
    let patch_files = Arc::clone(files);
    bus.register(move |request: PatchSkillFile| {
        let files = Arc::clone(&patch_files);
        async move {
            patch(
                &files,
                &request.path,
                &request.patches,
                request.description.as_deref(),
            )
            .await
            .map_err(bus_error)
        }
    })?;
    let write_files = Arc::clone(files);
    bus.register(move |request: WriteNewSkillFile| {
        let files = Arc::clone(&write_files);
        async move {
            write_new(&files, &request.path, request.content)
                .await
                .map_err(bus_error)
        }
    })?;
    Ok(())
}

/// Require and register the skill file procedures. The caller seals the bus.
pub fn bind_skill_files(bus: &CallBus) -> std::result::Result<(), String> {
    bus.require::<ReadSkillFile>()
        .map_err(|error| error.to_string())?;
    bus.require::<CreateSkillFile>()
        .map_err(|error| error.to_string())?;
    bus.require::<ReplaceSkillFile>()
        .map_err(|error| error.to_string())?;
    bus.require::<PatchSkillFile>()
        .map_err(|error| error.to_string())?;
    bus.require::<WriteNewSkillFile>()
        .map_err(|error| error.to_string())?;
    let files = SkillFiles::new()?;
    register_skill_files(bus, &files).map_err(|error| error.to_string())?;
    Ok(())
}

pub(crate) async fn write_unpacked_skill_files(
    bus: &CallBus,
    receiver: &mut tokio::sync::mpsc::Receiver<(std::path::PathBuf, Vec<u8>)>,
) -> std::result::Result<(), String> {
    while let Some((path, bytes)) = receiver.recv().await {
        let content = String::from_utf8(bytes)
            .map_err(|_| format!("SKILL.md is not UTF-8: {}", path.display()))?;
        bus.call(WriteNewSkillFile { path, content })
            .await
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// Build a sealed bus whose only procedures are the skill file operations.
pub fn open_skill_bus() -> std::result::Result<Arc<CallBus>, String> {
    let bus = CallBus::new();
    bind_skill_files(&bus)?;
    bus.seal().map_err(|error| error.to_string())?;
    Ok(bus)
}

pub(crate) async fn read_on(
    bus: &CallBus,
    path: &Path,
    max_bytes: Option<u64>,
) -> Result<SkillContent> {
    bus.call(ReadSkillFile {
        path: path.to_path_buf(),
        max_bytes,
        mode: SkillFileRead::Detect,
    })
    .await
    .map_err(from_call)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn round_trip_preserves_unknown_scalar_bytes_and_body() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let path = tmp.path().join("SKILL.md");
        let files = SkillFiles::new().map_err(Error::Validation)?;
        create(
            &files,
            &path,
            SkillFields {
                name: "demo".into(),
                description: "first".into(),
                body: "hello\n".into(),
            },
            Some("agent1"),
        )
        .await?;
        let created = std::fs::read_to_string(&path)?;
        assert!(created.contains("denied_agents:\n"));
        assert!(created.contains("allowed_agents:\n- agent1\n"));
        assert!(created.ends_with("---\nhello\n"));
        let read_back = read(&files, &path, None, SkillFileRead::Detect).await?;
        assert_eq!(read_back.body, "hello\n");
        assert_eq!(read_back.metadata.allowed_agents, ["agent1"]);
        std::fs::write(
            &path,
            "---\nname: demo\ndescription: first\n# needs docker\nbuild: 0x10\nversion: 1.10\n# note\ncustom: 1\n---\nhello\n",
        )?;
        replace(&files, &path, SkillFields {
            name: "demo".into(),
            description: "second".into(),
            body: read_back.body,
        })
        .await?;
        let updated = std::fs::read_to_string(&path)?;
        assert!(updated.contains("build: 0x10\n"));
        assert!(updated.contains("version: 1.10\n"));
        assert!(updated.contains("# needs docker\n"));
        assert!(updated.contains("# note\n"));
        assert!(updated.contains("custom: 1\n"));
        assert_eq!(
            read(&files, &path, None, SkillFileRead::Detect).await?.body,
            "hello\n"
        );
        assert_eq!(
            read(&files, &path, None, SkillFileRead::Detect)
                .await?
                .metadata
                .allowed_agents,
            Vec::<String>::new()
        );
        Ok(())
    }

    #[test]
    fn interpret_uses_frontmatter_slug_for_invalid_name() -> Result<()> {
        let content = "---\nname: \"SEO (Audit)\"\nslug: seo\ndescription: tools\n---\nbody\n";
        let parsed = interpret(content, Path::new("SKILL.md"))?;
        assert_eq!(parsed.metadata.name, "seo");
        assert_eq!(parsed.metadata.display_name.as_deref(), Some("SEO (Audit)"));
        Ok(())
    }

    #[test]
    fn interpret_rejects_invalid_name_without_slug() -> Result<()> {
        let content = "---\nname: \"Bad Name\"\n---\nbody\n";
        let Err(error) = interpret(content, Path::new("SKILL.md")) else {
            return Err(Error::Validation("expected missing slug".into()));
        };
        if error.to_string().contains("no slug") {
            Ok(())
        } else {
            Err(Error::Validation(error.to_string()))
        }
    }

    #[test]
    fn interpret_rejects_skill_md_without_frontmatter() -> Result<()> {
        let Err(error) = interpret("# Body\n", Path::new("SKILL.md")) else {
            return Err(Error::Validation("expected missing frontmatter".into()));
        };
        if error.to_string().contains("frontmatter") {
            Ok(())
        } else {
            Err(Error::Validation(error.to_string()))
        }
    }

    #[test]
    fn interpret_returns_unclosed_non_skill_markdown_as_text() -> Result<()> {
        let content = "---\ndescription: Review\nno closing fence\n";
        let parsed = interpret(content, Path::new("review.md"))?;
        assert_eq!(parsed.body, content);
        Ok(())
    }

    #[test]
    fn interpret_returns_invalid_yaml_non_skill_markdown_as_catalog_text() -> Result<()> {
        let content = "---\ndescription: Context: The user\n---\nbody\n";
        let path = Path::new("code-reviewer.md");
        assert!(interpret_mode(content, path, SkillFileRead::Detect).is_err());
        let parsed = interpret_mode(content, path, SkillFileRead::Catalog)?;
        assert_eq!(parsed.body, content);
        Ok(())
    }

    #[test]
    fn interpret_rejects_invalid_name_and_invalid_slug() -> Result<()> {
        let content = "---\nname: \"Bad Name\"\nslug: \"Also Bad\"\n---\nbody\n";
        let Err(error) = interpret(content, Path::new("SKILL.md")) else {
            return Err(Error::Validation("expected invalid slug".into()));
        };
        if error.to_string().contains("also invalid") {
            Ok(())
        } else {
            Err(Error::Validation(error.to_string()))
        }
    }

    #[tokio::test]
    async fn replace_preserves_nested_unknown_blocks() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let path = tmp.path().join("SKILL.md");
        let original = "---\nname: songsee\ndescription: Generate spectrograms\nrequires:\n  bins: [songsee]\nmetadata:\n  openclaw:\n    requires:\n      bins: [himalaya]\ndockerfile: Dockerfile\n---\nInstructions.\n";
        std::fs::write(&path, original)?;
        let files = SkillFiles::new().map_err(Error::Validation)?;
        replace(&files, &path, SkillFields {
            name: "songsee".into(),
            description: "Generate spectrograms".into(),
            body: "Instructions.\n".into(),
        })
        .await?;
        let updated = std::fs::read_to_string(&path)?;
        assert!(updated.contains("requires:\n  bins: [songsee]\n"));
        assert!(
            updated.contains("metadata:\n  openclaw:\n    requires:\n      bins: [himalaya]\n")
        );
        assert!(updated.contains("dockerfile: Dockerfile\n"));
        Ok(())
    }

    #[tokio::test(flavor = "current_thread")]
    #[allow(clippy::unwrap_used)]
    async fn skill_file_queue_runs_waiters_in_arrival_order() {
        let files = SkillFiles::new().unwrap();
        let order = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let hold_files = Arc::clone(&files);
        let hold = tokio::spawn(async move {
            with_file(&hold_files, move || {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(())
            })
            .await
            .unwrap();
        });
        started_rx.await.unwrap();
        let mut tasks = Vec::new();
        for number in [1_u8, 2] {
            let files = Arc::clone(&files);
            let order = Arc::clone(&order);
            let (queued_tx, queued_rx) = tokio::sync::oneshot::channel();
            let task = tokio::spawn(async move {
                queued_tx.send(()).unwrap();
                with_file(&files, move || {
                    order.lock().unwrap().push(number);
                    Ok(())
                })
                .await
                .unwrap();
            });
            queued_rx.await.unwrap();
            tokio::task::yield_now().await;
            tasks.push(task);
        }
        release_tx.send(()).unwrap();
        hold.await.unwrap();
        for task in tasks {
            task.await.unwrap();
        }
        assert_eq!(*order.lock().unwrap(), vec![1, 2]);
    }
}
