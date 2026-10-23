//! Skills system: discovery and parsing.
//!
//! Skills are directories containing a `SKILL.md` file with YAML frontmatter
//! and markdown instructions, following the Agent Skills open standard.

pub mod access;
pub mod discover;
pub mod error;
pub mod parse;
pub mod prompt_gen;
pub mod safety;
pub mod skill_file;
mod skill_handlers;
mod skill_sidecars;
pub mod types;
pub mod usage;

#[cfg(feature = "bundled-skills")]
pub mod bundled;

pub use {access::visible_to_agent, error::Error};

/// Sidecar subdirectories used by the prompt generator and the skills handler.
pub const SIDECAR_SUBDIRS: &[&str] = &["references", "templates", "assets", "scripts"];
#[cfg(feature = "file-watcher")]
pub mod watcher;
