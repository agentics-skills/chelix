use std::collections::HashMap;

use async_trait::async_trait;

use crate::{
    discover::SkillDiscoverer,
    error::{Error, Result},
    types::{SkillContent, SkillMetadata},
};

/// Registry for managing discovered and installed skills.
#[async_trait]
pub trait SkillRegistry: Send + Sync {
    /// List metadata for all available skills.
    async fn list_skills(&self) -> Result<Vec<SkillMetadata>>;

    /// Load the full content of a skill by name.
    async fn load_skill(&self, name: &str) -> Result<SkillContent>;

    /// Install a skill from a source (e.g. git URL).
    async fn install_skill(&self, source: &str) -> Result<SkillMetadata>;

    /// Remove an installed skill by name.
    async fn remove_skill(&self, name: &str) -> Result<()>;
}

/// In-memory registry backed by a discoverer.
pub struct InMemoryRegistry {
    skills: HashMap<String, SkillMetadata>,
    bus: std::sync::Arc<chelix_call_bus::CallBus>,
}

impl InMemoryRegistry {
    /// Create a new empty registry.
    pub fn new(bus: std::sync::Arc<chelix_call_bus::CallBus>) -> Self {
        Self {
            skills: HashMap::new(),
            bus,
        }
    }

    /// Populate the registry from a discoverer.
    pub async fn from_discoverer(
        discoverer: &dyn SkillDiscoverer,
        bus: std::sync::Arc<chelix_call_bus::CallBus>,
    ) -> Result<Self> {
        let discovered = discoverer.discover().await?;
        let mut skills = HashMap::new();
        for meta in discovered {
            skills.insert(meta.name.clone(), meta);
        }
        Ok(Self { skills, bus })
    }

    /// Add a skill directly (useful for testing).
    pub fn insert(&mut self, meta: SkillMetadata) {
        self.skills.insert(meta.name.clone(), meta);
    }
}

#[async_trait]
impl SkillRegistry for InMemoryRegistry {
    async fn list_skills(&self) -> Result<Vec<SkillMetadata>> {
        Ok(self.skills.values().cloned().collect())
    }

    async fn load_skill(&self, name: &str) -> Result<SkillContent> {
        let meta = self
            .skills
            .get(name)
            .ok_or_else(|| Error::NotFound(format!("skill '{}' not found", name)))?;

        crate::skill_file::read_on(&self.bus, &meta.path.join("SKILL.md"), None).await
    }

    async fn install_skill(&self, _source: &str) -> Result<SkillMetadata> {
        Err(Error::Install(
            "install not supported on in-memory registry; use install::install_skill".into(),
        ))
    }

    async fn remove_skill(&self, name: &str) -> Result<()> {
        let meta = self
            .skills
            .get(name)
            .ok_or_else(|| Error::NotFound(format!("skill '{}' not found", name)))?;

        let path = &meta.path;
        if !path.exists() {
            return Err(Error::NotFound(format!(
                "skill directory does not exist: {}",
                path.display()
            )));
        }

        // Only allow removing registry-installed skills
        if meta.source != Some(crate::types::SkillSource::Registry) {
            return Err(Error::Validation(format!(
                "can only remove registry-installed skills, '{}' is {:?}",
                name, meta.source
            )));
        }

        tokio::fs::remove_dir_all(path).await?;
        Ok(())
    }
}

#[allow(clippy::unwrap_used, clippy::expect_used)]
#[cfg(test)]
mod tests {
    use {super::*, crate::types::SkillSource, std::path::PathBuf};

    #[tokio::test]
    async fn test_in_memory_registry_list_and_load() {
        let tmp = tempfile::tempdir().unwrap();
        let skill_dir = tmp.path().join("my-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: my-skill\ndescription: test\n---\n# Instructions\nDo things.\n",
        )
        .unwrap();

        let mut reg = InMemoryRegistry::new(crate::skill_file::open_skill_bus().unwrap());
        reg.insert(SkillMetadata {
            name: "my-skill".into(),
            description: "test".into(),
            path: skill_dir,
            source: Some(SkillSource::Project),
            ..Default::default()
        });

        let skills = reg.list_skills().await.unwrap();
        assert_eq!(skills.len(), 1);

        let content = reg.load_skill("my-skill").await.unwrap();
        assert!(content.body.contains("Do things"));
    }

    #[tokio::test]
    async fn test_load_nonexistent_skill() {
        let reg = InMemoryRegistry::new(crate::skill_file::open_skill_bus().unwrap());
        assert!(reg.load_skill("nope").await.is_err());
    }

    #[tokio::test]
    async fn test_remove_non_registry_skill_fails() {
        let mut reg = InMemoryRegistry::new(crate::skill_file::open_skill_bus().unwrap());
        reg.insert(SkillMetadata {
            name: "local".into(),
            path: PathBuf::from("/tmp/local"),
            source: Some(SkillSource::Project),
            ..Default::default()
        });
        assert!(reg.remove_skill("local").await.is_err());
    }
}
