use std::path::PathBuf;

use serde::{Deserialize, Deserializer, Serialize, de};

/// Largest skill file body accepted by a capped read.
pub const MAX_SKILL_FILE_BYTES: u64 = 262144;

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

/// Agent list: a missing value, null, or `[]` is empty. Any other shape is an error.
pub(crate) fn deserialize_agent_list<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_yaml::Value::deserialize(deserializer)?;
    match value {
        serde_yaml::Value::Null => Ok(Vec::new()),
        serde_yaml::Value::Sequence(items) => items
            .into_iter()
            .map(|item| match item {
                serde_yaml::Value::String(id) => Ok(id),
                _ => Err(de::Error::custom("agent id must be a string")),
            })
            .collect(),
        _ => Err(de::Error::custom(
            "agent list must be a sequence of strings",
        )),
    }
}
