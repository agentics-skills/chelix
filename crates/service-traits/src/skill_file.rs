//! Skill file contract. Callers use these procedures instead of the skills crate.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use chelix_call_bus::Procedure;

/// Largest skill file body accepted by a capped read.
pub const MAX_SKILL_FILE_BYTES: u64 = 262144;

/// Failure of a skill file procedure.
#[derive(Debug, thiserror::Error)]
pub enum SkillFileError {
    /// The file is not on disk.
    #[error("{0}")]
    NotFound(String),
    /// A create found an existing file and left it unchanged.
    #[error("{0}")]
    AlreadyExists(String),
    /// The file could not be read or written as a skill document.
    #[error("{0}")]
    Failed(String),
}

/// Where a skill was discovered from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SkillSource {
    /// Project-local: `<data_dir>/.chelix/skills/`
    Project,
    /// Personal: `<data_dir>/skills/`
    Personal,
    /// Bundled inside a plugin directory.
    Plugin,
    /// Installed from a registry.
    Registry,
    /// Embedded in the binary.
    Bundled,
}

/// Provenance copied from an external source.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SkillOrigin {
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
}

/// Known skill metadata.
///
/// `Default` leaves `name` empty. Callers set `name` explicitly.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SkillMetadata {
    pub name: String,
    #[serde(default)]
    pub slug: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub homepage: Option<String>,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub compatibility: Option<String>,
    #[serde(default)]
    pub allowed_agents: Vec<String>,
    #[serde(default)]
    pub denied_agents: Vec<String>,
    #[serde(default)]
    pub origin: Option<SkillOrigin>,
    #[serde(skip)]
    pub category: Option<String>,
    #[serde(skip)]
    pub path: PathBuf,
    #[serde(skip)]
    pub source: Option<SkillSource>,
}

/// Interpreted skill file: known metadata and the body bytes.
#[derive(Debug, Clone)]
pub struct SkillContent {
    pub metadata: SkillMetadata,
    pub body: String,
}

/// How a skill file read interprets the path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillFileRead {
    /// Exact `SKILL.md` is a skill document. Any other name is a loose document.
    Detect,
    /// Parse the file as a skill document regardless of its name.
    SkillDocument,
    /// Like `Detect`, except a non-`SKILL.md` file with invalid YAML returns the original text.
    Catalog,
}

/// Read one skill file through the process queue.
pub struct ReadSkillFile {
    pub path: PathBuf,
    pub max_bytes: Option<u64>,
    pub mode: SkillFileRead,
}

/// Create a skill file. An existing file is left unchanged.
pub struct CreateSkillFile {
    pub path: PathBuf,
    pub name: String,
    pub description: String,
    pub body: String,
    pub creator_agent_id: Option<String>,
}

/// Replace name, description, and body.
pub struct ReplaceSkillFile {
    pub path: PathBuf,
    pub name: String,
    pub description: String,
    pub body: String,
}

/// Patch the body of an existing skill file.
pub struct PatchSkillFile {
    pub path: PathBuf,
    pub patches: Vec<(String, String)>,
    pub description: Option<String>,
}

/// Create a skill file from a complete markdown document.
pub struct WriteNewSkillFile {
    pub path: PathBuf,
    pub content: String,
}

impl Procedure for ReadSkillFile {
    type Error = SkillFileError;
    type Output = SkillContent;
}

impl Procedure for CreateSkillFile {
    type Error = SkillFileError;
    type Output = SkillContent;
}

impl Procedure for ReplaceSkillFile {
    type Error = SkillFileError;
    type Output = SkillContent;
}

impl Procedure for PatchSkillFile {
    type Error = SkillFileError;
    type Output = SkillContent;
}

impl Procedure for WriteNewSkillFile {
    type Error = SkillFileError;
    type Output = SkillContent;
}
