//! Directory listing for the managed tools service.
//!
//! Lists direct children only. Directories have a trailing slash, text files
//! include their logical line count, and binary files include their byte size.

use {
    anyhow::{Context, Result, bail},
    content_inspector::inspect,
    futures::{StreamExt, TryStreamExt, stream},
    std::{
        fs::FileType,
        path::{Path, PathBuf},
    },
    tokio::{fs::File, io::AsyncReadExt},
    tracing::instrument,
};

const BINARY_INSPECTION_BYTES: usize = 515;
const FILE_READ_BUFFER_BYTES: usize = 8 * 1024;
const MAX_CONCURRENT_FILE_INSPECTIONS: usize = 32;

#[derive(Debug)]
struct DirectoryEntry {
    name: String,
    path: PathBuf,
    file_type: FileType,
}

#[derive(Debug, Default)]
struct LogicalLineCounter {
    line_breaks: u64,
    previous_was_carriage_return: bool,
    last_byte: Option<u8>,
}

impl LogicalLineCounter {
    fn push(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if self.previous_was_carriage_return && byte != b'\n' {
                self.line_breaks += 1;
            }
            if byte == b'\n' {
                self.line_breaks += 1;
            }
            self.previous_was_carriage_return = byte == b'\r';
            self.last_byte = Some(byte);
        }
    }

    fn finish(&self) -> u64 {
        let mut line_breaks = self.line_breaks;
        if self.previous_was_carriage_return {
            line_breaks += 1;
        }

        match self.last_byte {
            None => 0,
            Some(b'\n' | b'\r') => line_breaks,
            Some(_) => line_breaks + 1,
        }
    }
}

#[derive(Debug)]
struct FileInspection {
    byte_len: u64,
    line_count: u64,
    is_binary: bool,
}

#[instrument(skip_all, fields(path = %raw_path.trim()))]
pub(crate) async fn run_tool(raw_path: &str) -> Result<String> {
    let path = raw_path.trim();
    if path.is_empty() {
        bail!("path must be a non-empty string.");
    }
    let path = Path::new(path);
    if !path.is_absolute() {
        bail!("path must be absolute.");
    }

    list_directory(path).await
}

async fn list_directory(path: &Path) -> Result<String> {
    let entries = read_directory_entries(path).await?;
    if entries.is_empty() {
        return Ok("Folder is empty".into());
    }

    let formatted = stream::iter(entries)
        .map(format_directory_entry)
        .buffered(MAX_CONCURRENT_FILE_INSPECTIONS)
        .try_collect::<Vec<_>>()
        .await?;
    Ok(formatted.join("\n"))
}

async fn read_directory_entries(path: &Path) -> Result<Vec<DirectoryEntry>> {
    let mut directory = tokio::fs::read_dir(path)
        .await
        .with_context(|| format!("failed to read directory '{}'", path.display()))?;
    let mut entries = Vec::new();

    while let Some(entry) = directory
        .next_entry()
        .await
        .with_context(|| format!("failed to read an entry in '{}'", path.display()))?
    {
        let entry_path = entry.path();
        let file_type = entry.file_type().await.with_context(|| {
            format!(
                "failed to determine the type of directory entry '{}'",
                entry_path.display()
            )
        })?;
        let name = entry.file_name().into_string().map_err(|name| {
            anyhow::anyhow!(
                "directory entry name in '{}' is not valid UTF-8: {name:?}",
                path.display()
            )
        })?;
        entries.push(DirectoryEntry {
            name,
            path: entry_path,
            file_type,
        });
    }

    entries.sort_unstable_by(|left, right| left.name.cmp(&right.name));
    Ok(entries)
}

async fn format_directory_entry(entry: DirectoryEntry) -> Result<String> {
    if entry.file_type.is_dir() {
        return Ok(format!("{}/", entry.name));
    }
    if !entry.file_type.is_file() {
        return Ok(entry.name);
    }

    let inspection = inspect_file(&entry.path).await?;
    if inspection.is_binary {
        return Ok(format!(
            "{} (binary, {})",
            entry.name,
            format_binary_size(inspection.byte_len)
        ));
    }

    Ok(format!(
        "{} ({})",
        entry.name,
        format_line_count(inspection.line_count)
    ))
}

async fn inspect_file(path: &Path) -> Result<FileInspection> {
    let mut file = File::open(path)
        .await
        .with_context(|| format!("failed to open file '{}'", path.display()))?;
    let mut buffer = [0_u8; FILE_READ_BUFFER_BYTES];
    let mut prefix = Vec::with_capacity(BINARY_INSPECTION_BYTES);
    let mut byte_len = 0_u64;
    let mut line_counter = LogicalLineCounter::default();

    loop {
        let read = file
            .read(&mut buffer)
            .await
            .with_context(|| format!("failed to read file '{}'", path.display()))?;
        if read == 0 {
            break;
        }

        byte_len = byte_len
            .checked_add(read as u64)
            .context("file size exceeds the supported range")?;
        let bytes = &buffer[..read];
        let prefix_remaining = BINARY_INSPECTION_BYTES.saturating_sub(prefix.len());
        prefix.extend_from_slice(&bytes[..bytes.len().min(prefix_remaining)]);
        line_counter.push(bytes);
    }

    Ok(FileInspection {
        byte_len,
        line_count: line_counter.finish(),
        is_binary: inspect(&prefix).is_binary(),
    })
}

fn format_line_count(line_count: u64) -> String {
    let unit = if line_count == 1 {
        "line"
    } else {
        "lines"
    };
    format!("{line_count} {unit}")
}

fn format_binary_size(byte_len: u64) -> String {
    const KIBIBYTE: u64 = 1024;
    const MEBIBYTE: u64 = KIBIBYTE * 1024;

    if byte_len < KIBIBYTE {
        let unit = if byte_len == 1 {
            "byte"
        } else {
            "bytes"
        };
        return format!("{byte_len} {unit}");
    }
    if byte_len < MEBIBYTE {
        return format_scaled_size(byte_len, KIBIBYTE, "KB");
    }
    format_scaled_size(byte_len, MEBIBYTE, "MB")
}

fn format_scaled_size(byte_len: u64, unit_bytes: u64, unit: &str) -> String {
    if byte_len.is_multiple_of(unit_bytes) {
        return format!("{} {unit}", byte_len / unit_bytes);
    }
    format!("{:.1} {unit}", byte_len as f64 / unit_bytes as f64)
}

#[allow(clippy::unwrap_used, clippy::expect_used)]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logical_line_counter_matches_reference_line_endings() {
        let cases: &[(&[&[u8]], u64)] = &[
            (&[b""], 0),
            (&[b"single"], 1),
            (&[b"first\nsecond"], 2),
            (&[b"first\nsecond\n"], 2),
            (&[b"first\rsecond\r"], 2),
            (&[b"first\r", b"\nsecond\r", b"\n"], 2),
            (&[b"\r\r"], 2),
        ];

        for (chunks, expected) in cases {
            let mut counter = LogicalLineCounter::default();
            for chunk in *chunks {
                counter.push(chunk);
            }
            assert_eq!(counter.finish(), *expected);
        }
    }

    #[test]
    fn binary_sizes_match_reference_format() {
        assert_eq!(format_binary_size(0), "0 bytes");
        assert_eq!(format_binary_size(1), "1 byte");
        assert_eq!(format_binary_size(1023), "1023 bytes");
        assert_eq!(format_binary_size(1024), "1 KB");
        assert_eq!(format_binary_size(1025), "1.0 KB");
        assert_eq!(format_binary_size(1536), "1.5 KB");
        assert_eq!(format_binary_size(1024 * 1024), "1 MB");
        assert_eq!(format_binary_size(1024 * 1024 + 1), "1.0 MB");
    }

    #[tokio::test]
    async fn lists_entries_in_reference_format() {
        let directory = tempfile::tempdir().unwrap();
        tokio::fs::create_dir(directory.path().join("nested"))
            .await
            .unwrap();
        tokio::fs::write(directory.path().join("alpha.txt"), b"alpha\nbeta")
            .await
            .unwrap();
        tokio::fs::write(directory.path().join("cr-only.txt"), b"first\rsecond\r")
            .await
            .unwrap();
        tokio::fs::write(directory.path().join("crlf.txt"), b"first\r\nsecond\r\n")
            .await
            .unwrap();
        tokio::fs::write(directory.path().join("empty.txt"), b"")
            .await
            .unwrap();
        tokio::fs::write(directory.path().join("sample.bin"), [
            0x4d, 0x5a, 0x00, 0x03, 0x00, 0x00, 0xff, 0xfe,
        ])
        .await
        .unwrap();
        tokio::fs::write(directory.path().join("single.txt"), "одна строка")
            .await
            .unwrap();

        let result = run_tool(directory.path().to_str().unwrap()).await.unwrap();

        assert_eq!(
            result,
            "alpha.txt (2 lines)\ncr-only.txt (2 lines)\ncrlf.txt (2 lines)\nempty.txt (0 lines)\nnested/\nsample.bin (binary, 8 bytes)\nsingle.txt (1 line)"
        );
    }

    #[tokio::test]
    async fn reports_empty_directory() {
        let directory = tempfile::tempdir().unwrap();

        let result = run_tool(directory.path().to_str().unwrap()).await.unwrap();

        assert_eq!(result, "Folder is empty");
    }

    #[tokio::test]
    async fn rejects_empty_path() {
        let error = run_tool("  ").await.unwrap_err();

        assert_eq!(error.to_string(), "path must be a non-empty string.");
    }

    #[tokio::test]
    async fn rejects_relative_path() {
        let error = run_tool("relative/directory").await.unwrap_err();

        assert_eq!(error.to_string(), "path must be absolute.");
    }

    #[tokio::test]
    async fn surfaces_missing_directory_error() {
        let error = run_tool("/definitely/not/a/real/list-directory-path")
            .await
            .unwrap_err();

        assert!(error.to_string().contains("failed to read directory"));
    }

    #[tokio::test]
    async fn surfaces_non_directory_error() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("file.txt");
        tokio::fs::write(&file, "content").await.unwrap();

        let error = run_tool(file.to_str().unwrap()).await.unwrap_err();

        assert!(error.to_string().contains("failed to read directory"));
    }
}
