use {
    crate::{
        error::Result,
        skill_file::SkillFiles,
        types::{SkillMetadata, SkillSource},
    },
    async_trait::async_trait,
    std::{
        path::{Path, PathBuf},
        sync::Arc,
    },
};

/// Discovers skills from filesystem paths.
#[async_trait]
pub trait SkillDiscoverer: Send + Sync {
    async fn discover(&self) -> Result<Vec<SkillMetadata>>;
}

/// Filesystem skill discovery in search-path order.
pub struct FsSkillDiscoverer {
    search_paths: Vec<(PathBuf, SkillSource)>,
    files: Arc<SkillFiles>,
}

impl FsSkillDiscoverer {
    pub fn new(search_paths: Vec<(PathBuf, SkillSource)>, files: Arc<SkillFiles>) -> Self {
        Self {
            search_paths,
            files,
        }
    }

    pub fn default_paths() -> Vec<(PathBuf, SkillSource)> {
        Self::default_paths_for(&chelix_config::data_dir())
    }

    #[must_use]
    pub fn default_paths_for(root: &Path) -> Vec<(PathBuf, SkillSource)> {
        vec![
            (root.join(".chelix/skills"), SkillSource::Project),
            (root.join("skills"), SkillSource::Personal),
        ]
    }
}

#[async_trait]
impl SkillDiscoverer for FsSkillDiscoverer {
    async fn discover(&self) -> Result<Vec<SkillMetadata>> {
        let mut skills = Vec::new();
        for (path, source) in &self.search_paths {
            match source {
                SkillSource::Personal | SkillSource::Project => {
                    discover_flat(&self.files, path, source, &mut skills).await?
                },
                // Bundled records come from BundledSkillStore, not from this scan.
                SkillSource::Bundled => {},
            }
        }
        Ok(skills)
    }
}

pub(crate) async fn discover_flat(
    files: &SkillFiles,
    path: &Path,
    source: &SkillSource,
    skills: &mut Vec<SkillMetadata>,
) -> Result<()> {
    let entries = match std::fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let dir = entry?.path();
        if !std::fs::metadata(&dir)?.is_dir() {
            continue;
        }
        let path = dir.join("SKILL.md");
        match std::fs::metadata(&path) {
            Ok(meta) if meta.is_file() => {},
            Ok(_) => continue,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        }
        let mut meta = crate::skill_file::read(files, &path, None).await?.metadata;
        meta.source = Some(source.clone());
        skills.push(meta);
    }
    Ok(())
}
#[allow(clippy::unwrap_used, clippy::expect_used)]
#[cfg(test)]
mod tests {
    use super::*;

    fn bus() -> Arc<SkillFiles> {
        SkillFiles::new().unwrap()
    }

    #[test]
    fn default_paths_for_returns_expected_layout() {
        // Regression guard: the gateway wires `ReadSkillTool` through this
        // helper, so the shape of the returned list is part of the public
        // contract. If someone reorders or renames any of these paths, the
        // `<available_skills>` prompt block and the read tool could start
        // disagreeing about which directories contain skills.
        let data_dir = PathBuf::from("/tmp/data");
        let paths = FsSkillDiscoverer::default_paths_for(&data_dir);
        assert_eq!(paths.len(), 2);
        assert_eq!(paths[0].0, PathBuf::from("/tmp/data/.chelix/skills"));
        assert_eq!(paths[0].1, SkillSource::Project);
        assert_eq!(paths[1].0, PathBuf::from("/tmp/data/skills"));
        assert_eq!(paths[1].1, SkillSource::Personal);
    }

    #[test]
    fn default_paths_matches_default_paths_for_with_data_dir() {
        // The zero-arg helper must reduce to the explicit-`data_dir`
        // variant applied to `chelix_config::data_dir()`. Any future
        // refactor that breaks this symmetry would cause the prompt
        // builder and the read tool to see different filesystem layouts.
        let explicit = FsSkillDiscoverer::default_paths_for(&chelix_config::data_dir());
        let implicit = FsSkillDiscoverer::default_paths();
        assert_eq!(explicit, implicit);
    }

    #[tokio::test]
    async fn test_discover_skills_in_temp_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let skills_dir = tmp.path().join("skills");
        std::fs::create_dir_all(skills_dir.join("my-skill")).unwrap();
        std::fs::write(
            skills_dir.join("my-skill/SKILL.md"),
            "---\nname: my-skill\ndescription: test\n---\nbody\n",
        )
        .unwrap();

        let discoverer =
            FsSkillDiscoverer::new(vec![(skills_dir.clone(), SkillSource::Project)], bus());
        let skills = discoverer.discover().await.unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "my-skill");
        assert_eq!(skills[0].source, Some(SkillSource::Project));
    }

    #[tokio::test]
    async fn test_discover_skips_missing_dirs() {
        let discoverer = FsSkillDiscoverer::new(
            vec![(PathBuf::from("/nonexistent/path"), SkillSource::Personal)],
            bus(),
        );
        let skills = discoverer.discover().await.unwrap();
        assert!(skills.is_empty());
    }

    #[tokio::test]
    async fn test_discover_skips_dirs_without_skill_md() {
        let tmp = tempfile::tempdir().unwrap();
        let skills_dir = tmp.path().join("skills");
        std::fs::create_dir_all(skills_dir.join("not-a-skill")).unwrap();
        std::fs::write(skills_dir.join("not-a-skill/README.md"), "hello").unwrap();

        let discoverer = FsSkillDiscoverer::new(vec![(skills_dir, SkillSource::Project)], bus());
        let skills = discoverer.discover().await.unwrap();
        assert!(skills.is_empty());
    }

    #[tokio::test]
    async fn discovery_excludes_invalid_access_lists() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("demo");
        std::fs::create_dir_all(&dir).unwrap();
        let discoverer = FsSkillDiscoverer::new(
            vec![(tmp.path().to_path_buf(), SkillSource::Personal)],
            bus(),
        );
        for declaration in ["allowed_agents: hello", "denied_agents: [2]"] {
            std::fs::write(
                dir.join("SKILL.md"),
                format!("---\nname: demo\n{declaration}\n---\nbody"),
            )
            .unwrap();
            assert!(discoverer.discover().await.is_err());
        }
        for declaration in ["allowed_agents:", "denied_agents:"] {
            std::fs::write(
                dir.join("SKILL.md"),
                format!("---\nname: demo\n{declaration}\n---\nbody"),
            )
            .unwrap();
            assert_eq!(discoverer.discover().await.unwrap().len(), 1);
        }
    }

    #[tokio::test]
    async fn test_discover_rejects_invalid_frontmatter() {
        let tmp = tempfile::tempdir().unwrap();
        let skills_dir = tmp.path().join("skills");
        std::fs::create_dir_all(skills_dir.join("bad-skill")).unwrap();
        std::fs::write(skills_dir.join("bad-skill/SKILL.md"), "no frontmatter here").unwrap();

        let discoverer = FsSkillDiscoverer::new(vec![(skills_dir, SkillSource::Project)], bus());
        let error = discoverer.discover().await.err().unwrap();
        assert!(error.to_string().contains("frontmatter"));
    }
}
