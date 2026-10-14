use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;

use crate::{
    error::Result,
    types::{SkillMetadata, SkillSource},
};

/// Discovers skills from filesystem paths.
#[async_trait]
pub trait SkillDiscoverer: Send + Sync {
    /// Scan configured paths and return metadata for all discovered skills.
    async fn discover(&self) -> Result<Vec<SkillMetadata>>;
}

/// Default filesystem-based skill discoverer.
pub struct FsSkillDiscoverer {
    /// (path, source) pairs to scan, in priority order.
    search_paths: Vec<(PathBuf, SkillSource)>,
    bus: Arc<chelix_call_bus::CallBus>,
}

impl FsSkillDiscoverer {
    pub fn new(
        search_paths: Vec<(PathBuf, SkillSource)>,
        bus: Arc<chelix_call_bus::CallBus>,
    ) -> Self {
        Self { search_paths, bus }
    }

    /// Build the default search paths for skill discovery.
    ///
    /// Workspace root is always the configured data directory.
    pub fn default_paths() -> Vec<(PathBuf, SkillSource)> {
        Self::default_paths_for(&chelix_config::data_dir())
    }

    /// Build the default search paths rooted at an explicit workspace / data
    /// directory.
    ///
    /// Prefer this over [`default_paths`](Self::default_paths) when the caller
    /// already has a `data_dir` in hand (e.g. the gateway's `bootstrap` scope)
    /// so the read and write sides stay consistent even if
    /// `chelix_config::data_dir()` is ever reconfigured at runtime.
    #[must_use]
    pub fn default_paths_for(data_dir: &Path) -> Vec<(PathBuf, SkillSource)> {
        vec![
            (data_dir.join(".chelix/skills"), SkillSource::Project),
            (data_dir.join("skills"), SkillSource::Personal),
        ]
    }
}

#[async_trait]
impl SkillDiscoverer for FsSkillDiscoverer {
    async fn discover(&self) -> Result<Vec<SkillMetadata>> {
        let mut skills = Vec::new();

        for (base_path, source) in &self.search_paths {
            if !base_path.is_dir() {
                continue;
            }

            match source {
                // Project/Personal: scan one level deep (always enabled).
                SkillSource::Project | SkillSource::Personal => {
                    discover_flat(&self.bus, base_path, source, &mut skills).await;
                },
                // Bundled skills are handled by CompositeSkillDiscoverer,
                // not by filesystem path scanning.
                SkillSource::Bundled => {},
            }
        }

        Ok(skills)
    }
}

// ── Composite discoverer (fs + bundled) ─────────────────────────────────────

/// Discoverer that merges filesystem-discovered skills with bundled skills.
///
/// Bundled skills are appended at lowest priority: any user skill with the
/// same name takes precedence. This ensures users can override or shadow a
/// bundled skill by creating one with the same name in their personal or
/// project skills directory.
#[cfg(feature = "bundled-skills")]
pub struct CompositeSkillDiscoverer {
    inner: Box<dyn SkillDiscoverer>,
    bundled: Arc<crate::bundled::BundledSkillStore>,
}

#[cfg(feature = "bundled-skills")]
impl CompositeSkillDiscoverer {
    pub fn new(
        inner: Box<dyn SkillDiscoverer>,
        bundled: Arc<crate::bundled::BundledSkillStore>,
    ) -> Self {
        Self { inner, bundled }
    }
}

#[cfg(feature = "bundled-skills")]
#[async_trait]
impl SkillDiscoverer for CompositeSkillDiscoverer {
    async fn discover(&self) -> Result<Vec<SkillMetadata>> {
        let mut skills = self.inner.discover().await?;
        let mut seen: std::collections::HashSet<String> =
            skills.iter().map(|s| s.name.clone()).collect();
        for bundled in self.bundled.discover().await {
            if seen.insert(bundled.name.clone()) {
                skills.push(bundled);
            }
        }
        Ok(skills)
    }
}

// ── Filesystem scanning helpers ─────────────────────────────────────────────

/// Scan one level deep for SKILL.md dirs (project/personal sources).
async fn discover_flat(
    bus: &chelix_call_bus::CallBus,
    base_path: &Path,
    source: &SkillSource,
    skills: &mut Vec<SkillMetadata>,
) {
    let entries = match std::fs::read_dir(base_path) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let skill_dir = entry.path();
        if !skill_dir.is_dir() {
            continue;
        }
        let skill_md = skill_dir.join("SKILL.md");
        if !skill_md.is_file() {
            continue;
        }
        match crate::skill_file::read_on(bus, &skill_md, None).await {
            Ok(content) => {
                let mut meta = content.metadata;
                meta.source = Some(source.clone());
                tracing::debug!(
                    path = %skill_md.display(),
                    source = ?source,
                    name = %meta.name,
                    "loaded SKILL.md"
                );
                skills.push(meta);
            },
            Err(e) => {
                tracing::warn!(?skill_dir, %e, "failed to parse SKILL.md");
            },
        }
    }
}

#[allow(clippy::unwrap_used, clippy::expect_used)]
#[cfg(test)]
mod tests {
    use super::*;

    fn bus() -> Arc<chelix_call_bus::CallBus> {
        crate::skill_file::open_skill_bus().unwrap()
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
            assert!(discoverer.discover().await.unwrap().is_empty());
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
    async fn test_discover_skips_invalid_frontmatter() {
        let tmp = tempfile::tempdir().unwrap();
        let skills_dir = tmp.path().join("skills");
        std::fs::create_dir_all(skills_dir.join("bad-skill")).unwrap();
        std::fs::write(skills_dir.join("bad-skill/SKILL.md"), "no frontmatter here").unwrap();

        let discoverer = FsSkillDiscoverer::new(vec![(skills_dir, SkillSource::Project)], bus());
        let skills = discoverer.discover().await.unwrap();
        assert!(skills.is_empty());
    }
}
