use {
    crate::error::{Error, Result},
    chelix_call_bus::{
        MAX_SIDECAR_FILE_BYTES, MAX_SIDECAR_FILES_PER_CALL, MAX_SIDECAR_FILES_PER_SUBDIR,
        MAX_SIDECAR_TOTAL_BYTES, SkillLinkedFile, SkillSidecarBody, SkillSidecarWrite,
    },
    std::{
        collections::HashSet,
        path::{Component, Path, PathBuf},
    },
};

pub(crate) fn normalize_relative_skill_file_path(path: &str) -> Result<PathBuf> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err(Error::Validation("file path must not be empty".into()));
    }
    let candidate = Path::new(trimmed);
    if candidate.is_absolute() {
        return Err(Error::Validation("file path must be relative".into()));
    }
    let mut normalized = PathBuf::new();
    for component in candidate.components() {
        match component {
            Component::Normal(segment) => {
                let segment_str = segment
                    .to_str()
                    .ok_or_else(|| Error::Validation("file path must be valid UTF-8".into()))?;
                if segment_str.starts_with('.') {
                    return Err(Error::Validation(format!(
                        "hidden path components are not allowed: '{trimmed}'"
                    )));
                }
                normalized.push(segment);
            },
            Component::CurDir => {},
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(Error::Validation("path traversal is not allowed".into()));
            },
        }
    }
    let file_name = normalized
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| Error::Validation("file path must name a file".into()))?;
    if file_name.eq_ignore_ascii_case("SKILL.md") {
        return Err(Error::Validation(
            "SKILL.md must be managed with create_skill/update_skill".into(),
        ));
    }
    Ok(normalized)
}

pub(crate) async fn list_skill_sidecar_files(dir: &Path) -> Result<Vec<SkillLinkedFile>> {
    let mut out = Vec::new();
    for sub in crate::SIDECAR_SUBDIRS {
        let mut entries = match tokio::fs::read_dir(dir.join(sub)).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        let mut local = Vec::new();
        while let Some(entry) = entries.next_entry().await? {
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| Error::Validation("sidecar filename must be valid UTF-8".into()))?;
            if name.starts_with('.') {
                continue;
            }
            let meta = tokio::fs::metadata(entry.path()).await?;
            if meta.is_file() {
                local.push(SkillLinkedFile {
                    path: format!("{sub}/{name}"),
                    bytes: meta.len(),
                });
            }
        }
        local.sort_by(|a, b| a.path.cmp(&b.path));
        out.extend(local.into_iter().take(MAX_SIDECAR_FILES_PER_SUBDIR));
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

pub(crate) fn sidecar_body(path: String, raw: Vec<u8>) -> Result<SkillSidecarBody> {
    if raw.len() > MAX_SIDECAR_FILE_BYTES {
        return Err(Error::Validation(format!(
            "sidecar file '{path}' exceeds maximum size of {MAX_SIDECAR_FILE_BYTES} bytes"
        )));
    }
    let bytes = raw.len() as u64;
    let text = String::from_utf8(raw).ok();
    Ok(SkillSidecarBody {
        path,
        is_binary: text.is_none(),
        text,
        bytes,
    })
}

pub(crate) async fn read_sidecar(
    name: &str,
    dir: &Path,
    relative: &Path,
) -> Result<SkillSidecarBody> {
    let target = dir.join(relative);
    let path = relative.display().to_string();
    match tokio::fs::symlink_metadata(&target).await {
        Ok(_) => {},
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let files = list_skill_sidecar_files(dir).await?;
            let hint = if files.is_empty() {
                "(none)".into()
            } else {
                files
                    .iter()
                    .map(|file| file.path.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            return Err(Error::NotFound(format!(
                "sidecar file '{path}' not found in skill '{name}'. Available sidecar files: {hint}"
            )));
        },
        Err(error) => return Err(error.into()),
    }
    let target = tokio::fs::canonicalize(target).await?;
    let meta = tokio::fs::metadata(&target).await?;
    if !meta.is_file() {
        return Err(Error::Validation(format!(
            "sidecar path '{path}' is not a regular file"
        )));
    }
    if meta.len() > MAX_SIDECAR_FILE_BYTES as u64 {
        return Err(Error::Validation(format!(
            "sidecar file '{path}' exceeds maximum size of {MAX_SIDECAR_FILE_BYTES} bytes"
        )));
    }
    sidecar_body(path, tokio::fs::read(target).await?)
}

struct ValidatedSkillFile {
    relative_path: PathBuf,
    content: String,
}

fn validate_sidecar_files(files: Vec<SkillSidecarWrite>) -> Result<Vec<ValidatedSkillFile>> {
    if files.is_empty() {
        return Err(Error::Validation("at least one file is required".into()));
    }
    if files.len() > MAX_SIDECAR_FILES_PER_CALL {
        return Err(Error::Validation(format!(
            "too many files: maximum is {MAX_SIDECAR_FILES_PER_CALL}"
        )));
    }
    let mut total_bytes = 0;
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(files.len());
    for file in files {
        let relative_path = normalize_relative_skill_file_path(&file.relative_path)?;
        if file.relative_path.trim().split('/').any(str::is_empty) {
            return Err(Error::Validation("file path must name a file".into()));
        }
        if !seen.insert(relative_path.clone()) {
            return Err(Error::Validation(format!(
                "duplicate file path '{}'",
                relative_path.display()
            )));
        }
        if file.content.len() > MAX_SIDECAR_FILE_BYTES {
            return Err(Error::Validation(format!(
                "file '{}' exceeds maximum size of {MAX_SIDECAR_FILE_BYTES} bytes",
                relative_path.display()
            )));
        }
        total_bytes += file.content.len();
        if total_bytes > MAX_SIDECAR_TOTAL_BYTES {
            return Err(Error::Validation(format!(
                "total file content exceeds maximum size of {MAX_SIDECAR_TOTAL_BYTES} bytes"
            )));
        }
        out.push(ValidatedSkillFile {
            relative_path,
            content: file.content,
        });
    }
    Ok(out)
}

pub(crate) async fn write_sidecars(
    root: &Path,
    name: &str,
    skill_dir: &Path,
    files: Vec<SkillSidecarWrite>,
) -> Result<()> {
    let files = validate_sidecar_files(files)?;
    for file in &files {
        let target = skill_dir.join(&file.relative_path);
        let parent = target
            .parent()
            .ok_or_else(|| Error::Validation("invalid file path".into()))?;
        tokio::fs::create_dir_all(parent).await?;
        match tokio::fs::metadata(&target).await {
            Ok(meta) if meta.is_dir() => {
                return Err(Error::Validation(format!(
                    "target '{}' is a directory",
                    file.relative_path.display()
                )));
            },
            Ok(_) => {},
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
            Err(error) => return Err(error.into()),
        }
        tokio::fs::write(target, &file.content).await?;
    }
    let ts = u64::try_from(
        (time::OffsetDateTime::now_utc() - time::OffsetDateTime::UNIX_EPOCH).whole_milliseconds(),
    )
    .map_err(|error| Error::Validation(error.to_string()))?;
    let line = serde_json::json!({ "ts": ts, "event": "skills.sidecar_files.write", "details": {
        "skill": name, "files": files.iter().map(|file| serde_json::json!({ "path": file.relative_path.display().to_string(), "bytes": file.content.len() })).collect::<Vec<_>>()
    }}).to_string();
    let dir = root.join("logs");
    std::fs::create_dir_all(&dir)?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("security-audit.jsonl"))?;
    use std::io::Write as _;
    writeln!(file, "{line}")?;
    Ok(())
}
