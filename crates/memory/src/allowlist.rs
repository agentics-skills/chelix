//! Indexable memory files relative to `data_dir`.

use std::{
    fs,
    io::{self, ErrorKind},
    path::{Component, Path, PathBuf},
};

/// Absolutize `path` without requiring it to exist.
pub fn absolutize(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// True when `path` is one of:
/// - `MEMORY.md`
/// - `agents/<id>/MEMORY.md`
/// - `agents/<id>/memory/<name>.md`
#[must_use]
pub fn is_indexable_memory_path(data_dir: &Path, path: &Path) -> bool {
    let Some(parts) = relative_normal_parts(data_dir, path) else {
        return false;
    };
    match parts.as_slice() {
        [memory] if memory == "MEMORY.md" => true,
        [agents, id, memory]
            if agents == "agents" && memory == "MEMORY.md" && is_single_segment(id) =>
        {
            true
        },
        [agents, id, mem, name]
            if agents == "agents"
                && mem == "memory"
                && is_single_segment(id)
                && is_note_name(name) =>
        {
            true
        },
        _ => false,
    }
}

/// Discover allowlisted memory files. Missing optional paths are skipped.
/// Any other I/O error fails the whole discovery.
pub fn discover_indexable_memory_files(data_dir: &Path) -> io::Result<Vec<PathBuf>> {
    let data_dir = absolutize(data_dir);
    let mut files = Vec::new();

    let root_memory = data_dir.join("MEMORY.md");
    if let Some(metadata) = optional_metadata(&root_memory)?
        && metadata.is_file()
    {
        files.push(root_memory);
    }

    let agents_root = data_dir.join("agents");
    let Some(agents_metadata) = optional_metadata(&agents_root)? else {
        return Ok(files);
    };
    if !agents_metadata.is_dir() {
        return Ok(files);
    }

    for entry in fs::read_dir(&agents_root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let agent_dir = entry.path();
        let Some(id) = agent_dir.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !is_single_segment(id) {
            continue;
        }

        let agent_memory = agent_dir.join("MEMORY.md");
        if let Some(metadata) = optional_metadata(&agent_memory)?
            && metadata.is_file()
        {
            files.push(agent_memory);
        }

        let notes_dir = agent_dir.join("memory");
        let Some(notes_metadata) = optional_metadata(&notes_dir)? else {
            continue;
        };
        if !notes_metadata.is_dir() {
            continue;
        }
        for note_entry in fs::read_dir(&notes_dir)? {
            let note_entry = note_entry?;
            if !note_entry.file_type()?.is_file() {
                continue;
            }
            let note_path = note_entry.path();
            if is_indexable_memory_path(&data_dir, &note_path) {
                files.push(note_path);
            }
        }
    }

    Ok(files)
}

fn optional_metadata(path: &Path) -> io::Result<Option<fs::Metadata>> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn relative_normal_parts(data_dir: &Path, path: &Path) -> Option<Vec<String>> {
    let data_dir = absolutize(data_dir);
    let path = absolutize(path);
    let rel = path.strip_prefix(&data_dir).ok()?;
    let mut parts = Vec::new();
    for component in rel.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_str()?.to_string()),
            _ => return None,
        }
    }
    Some(parts)
}

fn is_single_segment(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains('/') && !name.contains('\\')
}

fn is_note_name(name: &str) -> bool {
    is_single_segment(name)
        && name.ends_with(".md")
        && name.len() > 3
        && !name[..name.len() - 3].starts_with('.')
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use {
        super::{discover_indexable_memory_files, is_indexable_memory_path},
        std::fs,
        tempfile::TempDir,
    };

    #[test]
    fn allowlist_matches_only_specified_paths() {
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path();

        assert!(is_indexable_memory_path(
            data_dir,
            &data_dir.join("MEMORY.md")
        ));
        assert!(is_indexable_memory_path(
            data_dir,
            &data_dir.join("agents").join("main").join("MEMORY.md")
        ));
        assert!(is_indexable_memory_path(
            data_dir,
            &data_dir
                .join("agents")
                .join("main")
                .join("memory")
                .join("notes.md")
        ));
        assert!(!is_indexable_memory_path(
            data_dir,
            &data_dir.join("memory.md")
        ));
        assert!(!is_indexable_memory_path(
            data_dir,
            &data_dir.join("agents").join("main").join("SOUL.md")
        ));
        assert!(!is_indexable_memory_path(
            data_dir,
            &data_dir.join("memory").join("other.md")
        ));
        assert!(!is_indexable_memory_path(
            data_dir,
            &data_dir
                .join("agents")
                .join("main")
                .join("memory")
                .join("nested")
                .join("notes.md")
        ));
    }

    #[test]
    fn discover_lists_allowlisted_files_and_skips_others() {
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path();
        let agent_dir = data_dir.join("agents").join("main");
        let notes_dir = agent_dir.join("memory");
        fs::create_dir_all(&notes_dir).unwrap();
        fs::write(data_dir.join("MEMORY.md"), "root").unwrap();
        fs::write(agent_dir.join("MEMORY.md"), "agent").unwrap();
        fs::write(notes_dir.join("notes.md"), "note").unwrap();
        fs::write(agent_dir.join("SOUL.md"), "soul").unwrap();
        fs::create_dir_all(data_dir.join("memory")).unwrap();
        fs::write(data_dir.join("memory").join("other.md"), "old").unwrap();

        let mut found = discover_indexable_memory_files(data_dir).unwrap();
        found.sort();
        assert_eq!(found, vec![
            data_dir.join("MEMORY.md"),
            agent_dir.join("MEMORY.md"),
            notes_dir.join("notes.md"),
        ]);
    }
}
