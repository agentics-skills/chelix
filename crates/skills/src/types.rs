use serde::{Deserialize, Deserializer, Serialize, de};

use crate::formats::PluginFormat;

// ── Skills manifest ──────────────────────────────────────────────────────────

/// Top-level manifest tracking installed repos and per-skill enabled state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillsManifest {
    pub version: u32,
    #[serde(default)]
    pub repos: Vec<RepoEntry>,
}

impl Default for SkillsManifest {
    fn default() -> Self {
        Self {
            version: 1,
            repos: Vec::new(),
        }
    }
}

impl SkillsManifest {
    pub fn add_repo(&mut self, entry: RepoEntry) {
        self.repos.push(entry);
    }

    pub fn remove_repo(&mut self, source: &str) {
        self.repos.retain(|r| r.source != source);
    }

    pub fn find_repo(&self, source: &str) -> Option<&RepoEntry> {
        self.repos.iter().find(|r| r.source == source)
    }

    pub fn find_repo_mut(&mut self, source: &str) -> Option<&mut RepoEntry> {
        self.repos.iter_mut().find(|r| r.source == source)
    }

    pub fn set_skill_enabled(&mut self, source: &str, skill_name: &str, enabled: bool) -> bool {
        if let Some(repo) = self.find_repo_mut(source)
            && let Some(skill) = repo.skills.iter_mut().find(|s| s.name == skill_name)
        {
            skill.enabled = enabled;
            return true;
        }
        false
    }

    pub fn set_skill_trusted(&mut self, source: &str, skill_name: &str, trusted: bool) -> bool {
        if let Some(repo) = self.find_repo_mut(source)
            && let Some(skill) = repo.skills.iter_mut().find(|s| s.name == skill_name)
        {
            skill.trusted = trusted;
            return true;
        }
        false
    }
}

/// A single cloned repository with its discovered skills.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoEntry {
    pub source: String,
    pub repo_name: String,
    pub installed_at_ms: u64,
    #[serde(default)]
    pub commit_sha: Option<String>,
    #[serde(default)]
    pub format: PluginFormat,
    #[serde(default)]
    pub quarantined: bool,
    #[serde(default)]
    pub quarantine_reason: Option<String>,
    #[serde(default)]
    pub provenance: Option<RepoProvenance>,
    pub skills: Vec<SkillState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoProvenance {
    pub original_source: String,
    #[serde(default)]
    pub original_commit_sha: Option<String>,
    #[serde(default)]
    pub imported_from: Option<String>,
    #[serde(default)]
    pub exported_at_ms: Option<u64>,
}

/// Per-skill enabled state within a repo.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillState {
    pub name: String,
    pub relative_path: String,
    #[serde(default = "default_trusted")]
    pub trusted: bool,
    pub enabled: bool,
}

fn default_trusted() -> bool {
    // Backward compatibility: manifests created before trust-gating should
    // continue to work without immediately disabling all installed skills.
    true
}

pub use chelix_service_traits::{SkillContent, SkillMetadata, SkillOrigin, SkillSource};

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

#[allow(clippy::unwrap_used, clippy::expect_used)]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_state_defaults_trusted_for_backward_compat() {
        let parsed: SkillState = serde_json::from_str(
            r#"{"name":"demo","relative_path":"repo/skills/demo","enabled":true}"#,
        )
        .unwrap();
        assert!(parsed.trusted);
    }
}
