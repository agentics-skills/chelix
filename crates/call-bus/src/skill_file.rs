//! Skill procedures and their shared request and response types.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::Procedure;

/// Largest skill file body accepted by a capped read.
pub const MAX_SKILL_FILE_BYTES: u64 = 262144;

/// Failure of a skill file procedure.
#[derive(Debug, thiserror::Error)]
pub enum SkillFileError {
    /// The requested file or directory was not found.
    #[error("{0}")]
    NotFound(String),
    /// A create found an existing file.
    #[error("{0}")]
    AlreadyExists(String),
    /// The file could not be read or written as a skill document.
    #[error("{0}")]
    Failed(String),
    /// Access to the requested skill was rejected.
    #[error("{0}")]
    Rejected(String),
}

/// Where a skill was discovered from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SkillSource {
    /// Project-local: `<data_dir>/.chelix/skills/`
    Project,
    /// Personal: `<data_dir>/skills/`
    Personal,
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

pub const MAX_SIDECAR_FILES_PER_CALL: usize = 32;
pub const MAX_SIDECAR_FILES_PER_SUBDIR: usize = 8;
pub const MAX_SIDECAR_FILE_BYTES: usize = 128 * 1024;
pub const MAX_SIDECAR_TOTAL_BYTES: usize = 512 * 1024;

#[derive(Debug)]
pub struct SkillLinkedFile {
    pub path: String,
    pub bytes: u64,
}

#[derive(Debug)]
pub struct SkillSidecarBody {
    pub path: String,
    pub text: Option<String>,
    pub is_binary: bool,
    pub bytes: u64,
}

#[derive(Debug)]
pub struct SkillRead {
    pub metadata: SkillMetadata,
    pub body: String,
    pub linked_files: Vec<SkillLinkedFile>,
    pub sidecar: Option<SkillSidecarBody>,
    pub skill_dir: Option<PathBuf>,
    pub skill_dir_name: Option<String>,
    pub enabled: bool,
}

pub enum SkillActor {
    Agent { id: String },
    Process,
}

pub enum SkillLocate {
    PersonalDir,
    ProjectDir,
    Discovered,
    Bundled,
}

/// Read a skill document or one of its linked resources.
pub struct ReadSkillFile {
    pub name: String,
    pub max_bytes: Option<u64>,
    pub actor: SkillActor,
    pub locate: SkillLocate,
    pub file_path: Option<String>,
}

/// Create a skill file. An existing file is left unchanged.
pub struct CreateSkillFile {
    pub name: String,
    pub description: String,
    pub body: String,
    pub creator_agent_id: Option<String>,
    pub actor: SkillActor,
}

pub enum ReplaceDir {
    RequireExisting,
    Create,
}

/// Replace name, description, and body.
pub struct ReplaceSkillFile {
    pub name: String,
    pub description: String,
    pub body: String,
    pub actor: SkillActor,
    pub dir: ReplaceDir,
}

/// Patch the body of an existing skill file.
pub struct PatchSkillFile {
    pub name: String,
    pub patches: Vec<(String, String)>,
    pub description: Option<String>,
    pub actor: SkillActor,
}

pub struct DeleteSkillDirectory {
    pub name: String,
    pub agent_id: String,
}

pub struct SkillSidecarWrite {
    pub relative_path: String,
    pub content: String,
}

pub struct WriteSkillSidecars {
    pub name: String,
    pub files: Vec<SkillSidecarWrite>,
    pub agent_id: String,
}

pub enum SkillView {
    Merged,
    Bundled,
}

pub enum ListSkills {
    Agent { id: String },
    Process { view: SkillView },
}

#[derive(Debug)]
pub struct ListedSkill {
    pub metadata: SkillMetadata,
    pub enabled: bool,
    pub category_enabled: bool,
}

pub struct SetBundledSkillEnabled {
    pub name: String,
    pub enabled: bool,
}

pub struct SetBundledCategoryEnabled {
    pub category: String,
    pub enabled: bool,
}

#[derive(Debug)]
pub struct BundledToggle {
    pub category: Option<String>,
    pub enabled: bool,
}

#[derive(Debug)]
pub struct SkillDirectory {
    pub path: PathBuf,
}

impl Procedure for ReadSkillFile {
    type Error = SkillFileError;
    type Output = SkillRead;
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

impl Procedure for DeleteSkillDirectory {
    type Error = SkillFileError;
    type Output = ();
}

impl Procedure for WriteSkillSidecars {
    type Error = SkillFileError;
    type Output = SkillDirectory;
}

impl Procedure for ListSkills {
    type Error = SkillFileError;
    type Output = Vec<ListedSkill>;
}

impl Procedure for SetBundledSkillEnabled {
    type Error = SkillFileError;
    type Output = BundledToggle;
}

impl Procedure for SetBundledCategoryEnabled {
    type Error = SkillFileError;
    type Output = BundledToggle;
}
