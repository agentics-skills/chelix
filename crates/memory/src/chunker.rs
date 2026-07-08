//! Text chunking for memory — re-exports from chelix-splitter.

// Re-export the public API so existing consumers of `chelix_memory::chunker` continue to work.
pub use chelix_splitter::{Chunk, chunk_content, chunk_markdown};
