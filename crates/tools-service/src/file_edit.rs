//! Shared exact-edit primitives and same-target serialization.

use {
    anyhow::{Context, Result, anyhow, bail},
    chelix_protocol::EditFileRecovery,
    std::{
        collections::HashMap,
        io::Write as _,
        path::{Path, PathBuf},
        sync::{Arc, Weak},
    },
    tokio::sync::{Mutex, Semaphore},
};

#[derive(Default)]
pub(crate) struct FileEditRuntime {
    path_permits: Mutex<HashMap<PathBuf, Weak<Semaphore>>>,
}

impl FileEditRuntime {
    async fn path_permit(&self, path: &Path) -> Arc<Semaphore> {
        let mut path_permits = self.path_permits.lock().await;
        path_permits.retain(|_, permit| permit.strong_count() > 0);
        if let Some(permit) = path_permits.get(path).and_then(Weak::upgrade) {
            return permit;
        }

        let permit = Arc::new(Semaphore::new(1));
        path_permits.insert(path.to_path_buf(), Arc::downgrade(&permit));
        permit
    }

    pub(crate) async fn run<T, F>(&self, requested_path: PathBuf, operation: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&Path) -> Result<T> + Send + 'static,
    {
        let path_to_resolve = requested_path.clone();
        let canonical_path = tokio::task::spawn_blocking(move || resolve_target(&path_to_resolve))
            .await
            .context("blocking edit target resolution failed")??;
        let path_permit = self.path_permit(&canonical_path).await;
        let _permit = path_permit
            .acquire_owned()
            .await
            .map_err(|_| anyhow!("edit serialization closed unexpectedly"))?;

        tokio::task::spawn_blocking(move || {
            let current_path = resolve_target(&requested_path)?;
            if current_path != canonical_path {
                bail!(
                    "filePath resolved to a different target while waiting to edit '{}'.",
                    requested_path.display()
                );
            }
            operation(&canonical_path)
        })
        .await
        .context("blocking file edit task failed")?
    }
}

#[derive(Debug, Clone)]
pub(crate) struct EditOutcome {
    pub(crate) content: String,
    pub(crate) replacements: usize,
    pub(crate) recovery: Option<EditFileRecovery>,
}

pub(crate) fn apply_edit(
    content: &str,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
) -> Result<EditOutcome> {
    if old_string.is_empty() {
        bail!("oldString must not be empty.");
    }
    if old_string == new_string {
        bail!("newString must differ from oldString.");
    }

    let match_count = content.matches(old_string).count();
    if match_count > 0 {
        return finish_edit(
            content,
            old_string,
            new_string,
            replace_all,
            match_count,
            None,
        );
    }

    if content.contains("\r\n") && old_string.contains('\n') && !old_string.contains("\r\n") {
        let crlf_old = old_string.replace('\n', "\r\n");
        let crlf_new = new_string.replace('\n', "\r\n");
        let crlf_count = content.matches(&crlf_old).count();
        if crlf_count > 0 {
            return finish_edit(
                content,
                &crlf_old,
                &crlf_new,
                replace_all,
                crlf_count,
                Some(EditFileRecovery::Crlf),
            );
        }
    }

    let normalized_old = normalize_smart_quotes(old_string);
    let normalized_content = normalize_smart_quotes(content);
    if normalized_old != old_string || normalized_content != content {
        let match_count = normalized_content.matches(&normalized_old).count();
        if match_count > 0 {
            enforce_uniqueness(match_count, replace_all, " after smart-quote normalization")?;
            return Ok(EditOutcome {
                content: splice_via_normalized(
                    content,
                    &normalized_content,
                    &normalized_old,
                    new_string,
                    replace_all,
                )?,
                replacements: if replace_all {
                    match_count
                } else {
                    1
                },
                recovery: Some(EditFileRecovery::SmartQuotes),
            });
        }
    }

    bail!("oldString not found in file; edit refused.")
}

fn finish_edit(
    content: &str,
    needle: &str,
    replacement: &str,
    replace_all: bool,
    match_count: usize,
    recovery: Option<EditFileRecovery>,
) -> Result<EditOutcome> {
    enforce_uniqueness(match_count, replace_all, "")?;
    Ok(EditOutcome {
        content: if replace_all {
            content.replace(needle, replacement)
        } else {
            content.replacen(needle, replacement, 1)
        },
        replacements: if replace_all {
            match_count
        } else {
            1
        },
        recovery,
    })
}

fn enforce_uniqueness(match_count: usize, replace_all: bool, match_context: &str) -> Result<()> {
    if match_count > 1 && !replace_all {
        bail!(
            "oldString matches {match_count} locations in the file{match_context}; refusing to edit. Supply a larger oldString with more context to make the match unique, or set replaceAll=true."
        );
    }
    Ok(())
}

fn normalize_smart_quotes(value: &str) -> String {
    value
        .replace(['\u{2018}', '\u{2019}'], "'")
        .replace(['\u{201C}', '\u{201D}'], "\"")
}

fn splice_via_normalized(
    content: &str,
    normalized_content: &str,
    normalized_old: &str,
    new_string: &str,
    replace_all: bool,
) -> Result<String> {
    let content_chars = content.char_indices().collect::<Vec<_>>();
    let old_char_count = normalized_old.chars().count();
    let mut result = String::with_capacity(content.len());
    let mut previous_byte = 0;

    for (normalized_byte, _) in normalized_content.match_indices(normalized_old) {
        let start_char = normalized_content[..normalized_byte].chars().count();
        let end_char = start_char + old_char_count;
        let start_byte = original_byte_offset(&content_chars, start_char, content.len())?;
        let end_byte = original_byte_offset(&content_chars, end_char, content.len())?;
        result.push_str(&content[previous_byte..start_byte]);
        result.push_str(new_string);
        previous_byte = end_byte;
        if !replace_all {
            break;
        }
    }
    result.push_str(&content[previous_byte..]);
    Ok(result)
}

fn original_byte_offset(
    content_chars: &[(usize, char)],
    char_offset: usize,
    content_len: usize,
) -> Result<usize> {
    if char_offset == content_chars.len() {
        return Ok(content_len);
    }
    content_chars
        .get(char_offset)
        .map(|(byte_offset, _)| *byte_offset)
        .ok_or_else(|| anyhow!("smart-quote match could not be mapped to the original file"))
}

pub(crate) fn read_utf8(path: &Path) -> Result<String> {
    std::fs::read_to_string(path)
        .with_context(|| format!("failed to read UTF-8 file '{}'", path.display()))
}

pub(crate) fn response_path(path: &Path) -> Result<String> {
    path.to_str()
        .context("resolved filePath contains invalid UTF-8.")
        .map(str::to_owned)
}

pub(crate) fn persist_atomic(path: &Path, content: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("file '{}' has no parent directory", path.display()))?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("failed to create temporary file in '{}'", parent.display()))?;
    temporary
        .write_all(content)
        .with_context(|| format!("failed to write temporary file for '{}'", path.display()))?;
    temporary
        .as_file()
        .sync_all()
        .with_context(|| format!("failed to sync temporary file for '{}'", path.display()))?;
    let current_path = resolve_target(path)?;
    if current_path != path {
        bail!(
            "file target changed before persisting edit '{}'.",
            path.display()
        );
    }
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("failed to replace file '{}'", path.display()))?;
    Ok(())
}

fn resolve_target(path: &Path) -> Result<PathBuf> {
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect file '{}'", path.display()))?;
    if metadata.file_type().is_symlink() {
        bail!("refusing to edit symbolic link '{}'", path.display());
    }
    if !metadata.is_file() {
        bail!("target is not a regular file: {}", path.display());
    }

    let canonical_path = std::fs::canonicalize(path)
        .with_context(|| format!("failed to resolve file '{}'", path.display()))?;
    let canonical_metadata = std::fs::metadata(&canonical_path).with_context(|| {
        format!(
            "failed to inspect resolved file '{}'",
            canonical_path.display()
        )
    })?;
    if !canonical_metadata.is_file() {
        bail!(
            "resolved target is not a regular file: {}",
            canonical_path.display()
        );
    }
    if canonical_path.to_str().is_none() {
        bail!("resolved filePath contains invalid UTF-8.");
    }
    Ok(canonical_path)
}
