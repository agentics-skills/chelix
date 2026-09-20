//! Trait for writing to memory files, shared by memory tools.
//!
//! Implementations handle path validation, size limits, and the actual I/O.

/// Result of a successful memory write.
#[derive(Debug)]
pub struct MemoryWriteResult {
    /// Storage-agnostic location identifier for the written content.
    /// For file-based writers this is the resolved absolute path; other
    /// implementations may return a database ID, URL, or similar.
    pub location: String,
    /// Total number of bytes written.
    pub bytes_written: usize,
}

/// Writes content to memory files with validation.
#[async_trait::async_trait]
pub trait MemoryWriter: Send + Sync {
    /// Write `content` to a memory file.
    ///
    /// `file` is a relative path like `"MEMORY.md"` or `"memory/notes.md"`.
    /// When `append` is `true`, `content` is appended to any existing file
    /// (separated by a blank line); otherwise the file is overwritten.
    ///
    /// Returns the location and the total bytes written.
    async fn write_memory(
        &self,
        file: &str,
        content: &str,
        append: bool,
    ) -> anyhow::Result<MemoryWriteResult>;
}
