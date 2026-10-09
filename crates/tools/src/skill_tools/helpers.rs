//! Shared helpers: path validation, sidecar I/O, frontmatter parsing, audit.

use std::{
    collections::HashSet,
    path::{Component, Path, PathBuf},
};

use serde_json::Value;

use {
    super::{MAX_SIDECAR_FILE_BYTES, MAX_SIDECAR_FILES_PER_CALL, MAX_SIDECAR_TOTAL_BYTES},
    crate::error::Error,
};

// ── Frontmatter helpers ─────────────────────────────────────

/// Split a SKILL.md file into its frontmatter block (including delimiters and
/// trailing newline) and the body. If there is no frontmatter, frontmatter_block
/// is empty.
pub(crate) fn split_frontmatter_body(raw: &str) -> (&str, &str) {
    if !raw.starts_with("---") {
        return ("", raw);
    }
    if let Some(end_idx) = raw[3..].find("\n---") {
        let closing_end = 3 + end_idx + 1 + 3;
        let after_closing = &raw[closing_end..];
        let body_start = if after_closing.starts_with("\n\n") {
            closing_end + 2
        } else if after_closing.starts_with('\n') {
            closing_end + 1
        } else {
            closing_end
        };
        (&raw[..body_start], &raw[body_start..])
    } else {
        ("", raw)
    }
}

/// Replace the existing frontmatter description value.
pub(crate) fn update_frontmatter_description(
    frontmatter: &str,
    new_desc: &str,
) -> anyhow::Result<String> {
    Ok(chelix_skills::parse::update_description(
        frontmatter,
        new_desc,
    )?)
}

pub(super) fn build_skill_md(
    name: &str,
    description: &str,
    body: &str,
    allowed_tools: &[String],
) -> anyhow::Result<String> {
    let description = serde_json::to_string(description)?;
    let mut frontmatter = format!("---\nname: {name}\ndescription: {description}\n");
    if !allowed_tools.is_empty() {
        frontmatter.push_str("allowed_tools:\n");
        for tool in allowed_tools {
            let tool = serde_json::to_string(tool)?;
            frontmatter.push_str(&format!("  - {tool}\n"));
        }
    }
    frontmatter.push_str("---\n\n");
    frontmatter.push_str(body);
    if !body.ends_with('\n') {
        frontmatter.push('\n');
    }
    Ok(frontmatter)
}

#[async_trait::async_trait]
pub(super) trait SkillWriteRecorder: Send + Sync {
    async fn record_write(&self, name: &str);
}

#[async_trait::async_trait]
impl SkillWriteRecorder for chelix_skills::usage::SkillUsageStore {
    async fn record_write(&self, name: &str) {
        Self::record_write(self, name).await;
    }
}

// ── Skill I/O ───────────────────────────────────────────────

pub(super) async fn validate_existing_skill_file(path: &Path) -> anyhow::Result<()> {
    match tokio::fs::read_to_string(path).await {
        Ok(content) => {
            let skill_dir = path
                .parent()
                .ok_or_else(|| Error::message("invalid skill directory"))?;
            chelix_skills::parse::parse_metadata(&content, skill_dir)?;
            Ok(())
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

pub(super) async fn write_skill(
    skill_dir: &Path,
    content: &str,
    on_create: Option<&str>,
) -> anyhow::Result<()> {
    let skill_file = skill_dir.join("SKILL.md");
    validate_existing_skill_file(&skill_file).await?;
    tokio::fs::create_dir_all(skill_dir).await?;
    chelix_skills::publish_markdown(&skill_file, content, on_create).await?;
    Ok(())
}

// ── Sidecar validation and I/O ──────────────────────────────

#[derive(Debug, Clone)]
pub(super) struct ValidatedSkillFile {
    pub(super) relative_path: PathBuf,
    pub(super) content: String,
}

pub(super) fn validate_sidecar_files(files: &[Value]) -> anyhow::Result<Vec<ValidatedSkillFile>> {
    if files.is_empty() {
        return Err(Error::message("at least one file is required").into());
    }
    if files.len() > MAX_SIDECAR_FILES_PER_CALL {
        return Err(Error::message(format!(
            "too many files: maximum is {MAX_SIDECAR_FILES_PER_CALL}"
        ))
        .into());
    }

    let mut total_bytes = 0usize;
    let mut seen_paths = HashSet::new();
    let mut validated = Vec::with_capacity(files.len());

    for file in files {
        let path = file
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::message("each file needs a string 'path'"))?;
        let content = file
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::message("each file needs a string 'content'"))?;

        let relative_path = normalize_relative_skill_file_path(path)?;
        if !seen_paths.insert(relative_path.clone()) {
            return Err(Error::message(format!(
                "duplicate file path '{}'",
                relative_path.display()
            ))
            .into());
        }

        let file_bytes = content.len();
        if file_bytes > MAX_SIDECAR_FILE_BYTES {
            return Err(Error::message(format!(
                "file '{}' exceeds maximum size of {MAX_SIDECAR_FILE_BYTES} bytes",
                relative_path.display()
            ))
            .into());
        }

        total_bytes += file_bytes;
        if total_bytes > MAX_SIDECAR_TOTAL_BYTES {
            return Err(Error::message(format!(
                "total file content exceeds maximum size of {MAX_SIDECAR_TOTAL_BYTES} bytes"
            ))
            .into());
        }

        validated.push(ValidatedSkillFile {
            relative_path,
            content: content.to_string(),
        });
    }

    Ok(validated)
}

pub(super) fn normalize_relative_skill_file_path(path: &str) -> anyhow::Result<PathBuf> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err(Error::message("file path must not be empty").into());
    }

    let candidate = Path::new(trimmed);
    if candidate.is_absolute() {
        return Err(Error::message("file path must be relative").into());
    }

    let mut normalized = PathBuf::new();
    for component in candidate.components() {
        match component {
            Component::Normal(segment) => {
                let Some(segment_str) = segment.to_str() else {
                    return Err(Error::message("file path must be valid UTF-8").into());
                };
                if segment_str.starts_with('.') {
                    return Err(Error::message(format!(
                        "hidden path components are not allowed: '{trimmed}'"
                    ))
                    .into());
                }
                normalized.push(segment);
            },
            Component::CurDir => {},
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(Error::message("path traversal is not allowed").into());
            },
        }
    }

    let Some(file_name) = normalized.file_name().and_then(|name| name.to_str()) else {
        return Err(Error::message("file path must name a file").into());
    };

    if file_name.eq_ignore_ascii_case("SKILL.md") {
        return Err(
            Error::message("SKILL.md must be managed with create_skill/update_skill").into(),
        );
    }

    Ok(normalized)
}

pub(super) async fn write_sidecar_files(
    skill_dir: &Path,
    files: &[ValidatedSkillFile],
) -> crate::Result<()> {
    for file in files {
        let target = skill_dir.join(&file.relative_path);
        let parent = target
            .parent()
            .ok_or_else(|| Error::message("invalid file path"))?;
        tokio::fs::create_dir_all(parent).await?;
        match tokio::fs::metadata(&target).await {
            Ok(metadata) if metadata.is_dir() => {
                return Err(Error::message(format!(
                    "target '{}' is a directory",
                    file.relative_path.display()
                )));
            },
            Ok(_) => {},
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
            Err(error) => return Err(error.into()),
        }
        tokio::fs::write(&target, &file.content).await?;
    }

    Ok(())
}

pub(super) fn audit_sidecar_file_write(
    data_dir: &Path,
    skill_name: &str,
    files: &[ValidatedSkillFile],
) {
    let dir = data_dir.join("logs");
    let path = dir.join("security-audit.jsonl");
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let line = serde_json::json!({
        "ts": now_ms,
        "event": "skills.sidecar_files.write",
        "details": {
            "skill": skill_name,
            "files": files.iter().map(|file| {
                serde_json::json!({
                    "path": file.relative_path.display().to_string(),
                    "bytes": file.content.len(),
                })
            }).collect::<Vec<_>>(),
        },
    })
    .to_string();

    if let Err(err) = (|| -> std::io::Result<()> {
        std::fs::create_dir_all(&dir)?;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        use std::io::Write as _;
        writeln!(file, "{line}")?;
        Ok(())
    })() {
        tracing::warn!(
            error = %err,
            skill = skill_name,
            "failed to write sidecar-file audit entry"
        );
    }
}
