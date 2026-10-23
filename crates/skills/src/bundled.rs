//! Bundled skill assets and executable sidecar materialization.

use {
    crate::{
        error::{Error, Result},
        skill_file::SkillFiles,
        types::{SkillMetadata, SkillSource},
    },
    std::{
        path::{Path, PathBuf},
        sync::Arc,
    },
};

static BUNDLED_ASSETS: include_dir::Dir<'static> =
    include_dir::include_dir!("$CARGO_MANIFEST_DIR/src/assets");

enum AssetSource {
    Filesystem(PathBuf),
    Embedded,
}

/// Stores bundled assets and materializes their executable resources.
pub struct BundledSkillStore {
    source: AssetSource,
    materialize_dir: PathBuf,
    files: Arc<SkillFiles>,
}

impl BundledSkillStore {
    #[must_use]
    pub fn new(files: Arc<SkillFiles>) -> Self {
        Self::with_materialize_dir(chelix_config::data_dir().join("bundled-skills"), files)
    }

    #[must_use]
    pub fn with_materialize_dir(materialize_dir: PathBuf, files: Arc<SkillFiles>) -> Self {
        let cargo_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/assets");
        let source = if cargo_dir.is_dir() {
            AssetSource::Filesystem(cargo_dir)
        } else {
            AssetSource::Embedded
        };
        Self {
            source,
            materialize_dir,
            files,
        }
    }

    pub async fn discover(&self) -> Result<Vec<SkillMetadata>> {
        match &self.source {
            AssetSource::Filesystem(dir) => discover_from_fs(&self.files, dir).await,
            AssetSource::Embedded => discover_from_embedded(),
        }
    }

    pub async fn read_skill(&self, name: &str) -> Option<String> {
        match &self.source {
            AssetSource::Filesystem(dir) => read_skill_body_fs(&self.files, dir, name).await,
            AssetSource::Embedded => read_skill_body_embedded(name),
        }
    }

    pub fn read_sidecar(&self, name: &str, rel_path: &str) -> Result<Option<(Vec<u8>, bool)>> {
        match &self.source {
            AssetSource::Filesystem(dir) => {
                let Some(skill_dir) = find_skill_dir_fs(dir, name)? else {
                    return Ok(None);
                };
                match std::fs::read(skill_dir.join(rel_path)) {
                    Ok(bytes) => {
                        let is_utf8 = std::str::from_utf8(&bytes).is_ok();
                        Ok(Some((bytes, is_utf8)))
                    },
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                    Err(error) => Err(error.into()),
                }
            },
            AssetSource::Embedded => Ok(read_sidecar_embedded(name, rel_path)),
        }
    }

    pub fn list_sidecars(&self, name: &str) -> Result<Vec<(String, u64)>> {
        match &self.source {
            AssetSource::Filesystem(dir) => list_sidecars_fs(dir, name),
            AssetSource::Embedded => Ok(list_sidecars_embedded(name)),
        }
    }

    pub fn materialize_sidecars(&self, name: &str) -> Result<Option<PathBuf>> {
        self.materialize_sidecars_to(name, &self.materialize_dir)
    }

    pub fn materialize_sidecars_to(
        &self,
        name: &str,
        target_dir: &Path,
    ) -> Result<Option<PathBuf>> {
        let sidecars = self.list_sidecars(name)?;
        if sidecars.is_empty() {
            return Ok(None);
        }
        let skill_dir = target_dir.join(name);
        for (path, _) in sidecars {
            let (bytes, _) = self.read_sidecar(name, &path)?.ok_or_else(|| Error::NotFound(format!("sidecar file '{path}' not found in bundled skill '{name}' during materialization")))?;
            let target = skill_dir.join(&path);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&target, bytes)?;
            #[cfg(unix)]
            if path.starts_with("scripts/") {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755))?;
            }
        }
        Ok(Some(skill_dir))
    }
}

async fn discover_from_fs(files: &SkillFiles, assets_dir: &Path) -> Result<Vec<SkillMetadata>> {
    let mut skills = Vec::new();
    discover_from_fs_recursive(files, assets_dir, assets_dir, &mut skills).await?;
    Ok(skills)
}

async fn discover_from_fs_recursive(
    files: &SkillFiles,
    assets_root: &Path,
    dir: &Path,
    skills: &mut Vec<SkillMetadata>,
) -> Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let path = entry?.path();
        if !std::fs::metadata(&path)?.is_dir() {
            continue;
        }
        let skill_md = path.join("SKILL.md");
        match std::fs::metadata(&skill_md) {
            Ok(meta) if meta.is_file() => {
                let mut meta = crate::skill_file::read(files, &skill_md, None)
                    .await?
                    .metadata;
                meta.source = Some(SkillSource::Bundled);
                meta.category = category_from_path(assets_root, &path);
                skills.push(meta);
            },
            Ok(_) => {},
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Box::pin(discover_from_fs_recursive(
                    files,
                    assets_root,
                    &path,
                    skills,
                ))
                .await?
            },
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn category_from_path(assets_root: &Path, skill_dir: &Path) -> Option<String> {
    Some(
        skill_dir
            .strip_prefix(assets_root)
            .ok()?
            .components()
            .next()?
            .as_os_str()
            .to_string_lossy()
            .into_owned(),
    )
}

async fn read_skill_body_fs(files: &SkillFiles, assets_dir: &Path, name: &str) -> Option<String> {
    let dir = find_skill_dir_fs(assets_dir, name)
        .inspect_err(
            |error| tracing::error!(skill = %name, %error, "failed to locate bundled skill"),
        )
        .ok()??;
    crate::skill_file::read(files, &dir.join("SKILL.md"), None)
        .await
        .inspect_err(
            |error| tracing::error!(skill = %name, %error, "failed to read bundled skill body"),
        )
        .ok()
        .map(|content| content.body)
}

fn find_skill_dir_fs(dir: &Path, name: &str) -> Result<Option<PathBuf>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let path = entry?.path();
        if !std::fs::metadata(&path)?.is_dir() {
            continue;
        }
        if path.file_name().and_then(|name| name.to_str()) == Some(name)
            && path.join("SKILL.md").is_file()
        {
            return Ok(Some(path));
        }
        if let Some(found) = find_skill_dir_fs(&path, name)? {
            return Ok(Some(found));
        }
    }
    Ok(None)
}

fn list_sidecars_fs(dir: &Path, name: &str) -> Result<Vec<(String, u64)>> {
    let Some(dir) = find_skill_dir_fs(dir, name)? else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for sub in crate::SIDECAR_SUBDIRS {
        let entries = match std::fs::read_dir(dir.join(sub)) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        for entry in entries {
            let entry = entry?;
            let meta = std::fs::metadata(entry.path())?;
            if !meta.is_file() {
                continue;
            }
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| Error::Validation("sidecar filename must be valid UTF-8".into()))?;
            if !name.starts_with('.') {
                out.push((format!("{sub}/{name}"), meta.len()));
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

fn dir_file_by_name<'a>(
    dir: &'a include_dir::Dir<'static>,
    name: &str,
) -> Option<&'a include_dir::File<'static>> {
    dir.files()
        .find(|file| file.path().file_name().and_then(|name| name.to_str()) == Some(name))
}

fn discover_from_embedded() -> Result<Vec<SkillMetadata>> {
    let mut skills = Vec::new();
    discover_from_embedded_recursive(&BUNDLED_ASSETS, &mut skills)?;
    Ok(skills)
}

fn discover_from_embedded_recursive(
    dir: &include_dir::Dir<'static>,
    skills: &mut Vec<SkillMetadata>,
) -> Result<()> {
    for subdir in dir.dirs() {
        if let Some(file) = dir_file_by_name(subdir, "SKILL.md") {
            let text = std::str::from_utf8(file.contents())
                .map_err(|error| Error::Parse(error.to_string()))?;
            let path = PathBuf::from("__bundled__").join(subdir.path());
            let mut meta = crate::skill_file::interpret(text, &path.join("SKILL.md"))?.metadata;
            meta.source = Some(SkillSource::Bundled);
            meta.category = subdir
                .path()
                .components()
                .next()
                .and_then(|part| part.as_os_str().to_str())
                .map(str::to_owned);
            skills.push(meta);
        } else {
            discover_from_embedded_recursive(subdir, skills)?;
        }
    }
    Ok(())
}

fn read_skill_body_embedded(name: &str) -> Option<String> {
    let dir = find_skill_dir_embedded(name)?;
    let file = dir_file_by_name(dir, "SKILL.md")?;
    let text = std::str::from_utf8(file.contents())
        .inspect_err(|error| tracing::error!(skill = %name, %error, "invalid bundled skill UTF-8"))
        .ok()?;
    crate::skill_file::interpret(
        text,
        &PathBuf::from("__bundled__")
            .join(dir.path())
            .join("SKILL.md"),
    )
    .inspect_err(
        |error| tracing::error!(skill = %name, %error, "failed to interpret bundled skill body"),
    )
    .ok()
    .map(|content| content.body)
}

fn read_sidecar_embedded(name: &str, path: &str) -> Option<(Vec<u8>, bool)> {
    let dir = find_skill_dir_embedded(name)?;
    let file = dir.get_file(dir.path().join(path))?;
    let bytes = file.contents().to_vec();
    let utf8 = std::str::from_utf8(&bytes).is_ok();
    Some((bytes, utf8))
}

fn list_sidecars_embedded(name: &str) -> Vec<(String, u64)> {
    let Some(dir) = find_skill_dir_embedded(name) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for sub in crate::SIDECAR_SUBDIRS {
        let Some(subdir) = dir
            .dirs()
            .find(|child| child.path() == dir.path().join(sub))
        else {
            continue;
        };
        for file in subdir.files() {
            if let Some(name) = file.path().file_name().and_then(|name| name.to_str())
                && !name.starts_with('.')
            {
                out.push((format!("{sub}/{name}"), file.contents().len() as u64));
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn find_skill_dir_embedded(name: &str) -> Option<&'static include_dir::Dir<'static>> {
    find_skill_dir_embedded_recursive(&BUNDLED_ASSETS, name)
}

fn find_skill_dir_embedded_recursive(
    dir: &'static include_dir::Dir<'static>,
    name: &str,
) -> Option<&'static include_dir::Dir<'static>> {
    for subdir in dir.dirs() {
        if subdir.path().file_name().and_then(|name| name.to_str()) == Some(name)
            && dir_file_by_name(subdir, "SKILL.md").is_some()
        {
            return Some(subdir);
        }
        if let Some(found) = find_skill_dir_embedded_recursive(subdir, name) {
            return Some(found);
        }
    }
    None
}
#[allow(clippy::unwrap_used, clippy::expect_used)]
#[cfg(test)]
mod tests {
    use {super::*, crate::parse};

    const EXPECTED_BUNDLED_SKILLS: &[(&str, &str)] = &[("mcp-servers", "devops")];

    fn store() -> BundledSkillStore {
        BundledSkillStore::new(SkillFiles::new().unwrap())
    }

    fn assert_expected_skills(skills: &[SkillMetadata]) {
        let mut actual = skills
            .iter()
            .map(|skill| {
                (
                    skill.name.as_str(),
                    skill.category.as_deref().unwrap_or_default(),
                )
            })
            .collect::<Vec<_>>();
        actual.sort_unstable();
        assert_eq!(actual, EXPECTED_BUNDLED_SKILLS);
    }

    fn sidecar_fixture_store(root: &Path) -> BundledSkillStore {
        let assets = root.join("assets");
        let skill_dir = assets.join("test/sidecar-fixture");
        let scripts_dir = skill_dir.join("scripts");
        std::fs::create_dir_all(&scripts_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: sidecar-fixture\ndescription: Test skill with a sidecar.\n---\n\n# Fixture\n",
        )
        .unwrap();
        std::fs::write(scripts_dir.join("fixture.sh"), "#!/bin/sh\necho fixture\n").unwrap();

        BundledSkillStore {
            source: AssetSource::Filesystem(assets),
            materialize_dir: root.join("materialized"),
            files: SkillFiles::new().unwrap(),
        }
    }

    async fn read_store(
        store: BundledSkillStore,
        root: &Path,
        name: &str,
    ) -> chelix_call_bus::SkillRead {
        let bus = chelix_call_bus::CallBus::new();
        let files = Arc::clone(&store.files);
        crate::skill_file::require_skill_files(&bus).unwrap();
        crate::skill_file::register_skill_files(
            &bus,
            &files,
            Some(Arc::new(store)),
            root.to_path_buf(),
            Arc::new(tokio::sync::RwLock::new(
                chelix_config::AgentsConfig::default(),
            )),
            Arc::new(tokio::sync::RwLock::new(
                chelix_config::schema::SkillsConfig::default(),
            )),
        )
        .unwrap();
        bus.seal().unwrap();
        bus.call(chelix_call_bus::ReadSkillFile {
            name: name.into(),
            max_bytes: None,
            actor: chelix_call_bus::SkillActor::Process,
            locate: chelix_call_bus::SkillLocate::Bundled,
            file_path: None,
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn bundled_read_without_scripts_has_empty_skill_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let store = BundledSkillStore::with_materialize_dir(
            tmp.path().join("materialized"),
            SkillFiles::new().unwrap(),
        );
        let content = read_store(store, tmp.path(), "mcp-servers").await;
        assert!(content.skill_dir.is_none());
        assert!(content.skill_dir_name.is_none());
        assert_eq!(content.metadata.category.as_deref(), Some("devops"));
        assert!(content.metadata.origin.is_some());
    }

    #[tokio::test]
    async fn bundled_read_with_scripts_materializes_skill_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let content = read_store(
            sidecar_fixture_store(tmp.path()),
            tmp.path(),
            "sidecar-fixture",
        )
        .await;
        let directory = content.skill_dir.unwrap();
        assert!(directory.join("scripts/fixture.sh").is_file());
        assert!(content.skill_dir_name.is_none());
        assert_eq!(content.linked_files[0].path, "scripts/fixture.sh");
    }

    #[test]
    fn embedded_assets_match_expected_skills() {
        let skills = discover_from_embedded().unwrap();
        assert_expected_skills(&skills);
    }

    #[test]
    fn every_embedded_skill_body_is_readable() {
        let skills = discover_from_embedded().unwrap();
        for skill in skills {
            let body = read_skill_body_embedded(&skill.name);
            assert!(
                body.as_ref().is_some_and(|body| !body.is_empty()),
                "embedded skill '{}' body not readable",
                skill.name
            );
        }
    }

    #[tokio::test]
    async fn filesystem_assets_match_expected_skills() {
        let skills = store().discover().await.unwrap();
        assert_expected_skills(&skills);
        for skill in &skills {
            assert_eq!(skill.source, Some(SkillSource::Bundled));
            assert!(!skill.name.is_empty(), "skill has empty name");
            assert!(
                !skill.description.is_empty(),
                "skill {} has empty description",
                skill.name
            );
        }
    }

    #[tokio::test]
    async fn no_duplicate_skill_names() {
        let skills = store().discover().await.unwrap();
        let mut seen = std::collections::HashSet::new();
        for skill in &skills {
            assert!(
                seen.insert(&skill.name),
                "duplicate bundled skill name: {}",
                skill.name
            );
        }
    }

    #[tokio::test]
    async fn bundled_skills_do_not_include_mcporter() {
        let skills = store().discover().await.unwrap();
        assert!(
            skills.iter().all(|skill| skill.name != "mcporter"),
            "Chelix has native MCP tools; mcporter must not be bundled by default"
        );
    }

    #[tokio::test]
    async fn all_names_pass_validation() {
        let skills = store().discover().await.unwrap();
        for skill in &skills {
            assert!(
                parse::validate_name(&skill.name),
                "skill name '{}' fails validation",
                skill.name
            );
        }
    }

    #[tokio::test]
    async fn every_bundled_skill_has_category() {
        let skills = store().discover().await.unwrap();
        for skill in &skills {
            assert!(
                skill.category.is_some(),
                "skill '{}' has no category",
                skill.name
            );
            assert!(
                !skill.category.as_ref().is_none_or(String::is_empty),
                "skill '{}' has empty category",
                skill.name
            );
        }
    }

    #[tokio::test]
    async fn bundled_categories_match_remaining_assets() {
        let skills = store().discover().await.unwrap();
        let cats: std::collections::HashSet<String> =
            skills.iter().filter_map(|s| s.category.clone()).collect();
        let expected = ["devops"].into_iter().map(String::from).collect();
        assert_eq!(cats, expected);
    }

    #[tokio::test]
    async fn category_derived_from_top_level_directory() {
        let skills = store().discover().await.unwrap();
        for (name, category) in EXPECTED_BUNDLED_SKILLS {
            let skill = skills
                .iter()
                .find(|skill| skill.name == *name)
                .unwrap_or_else(|| panic!("{name} should be bundled"));
            assert_eq!(skill.category.as_deref(), Some(*category));
        }
    }

    #[tokio::test]
    async fn all_bundled_skills_have_origin() {
        let skills = store().discover().await.unwrap();
        for skill in &skills {
            assert!(
                skill.origin.is_some(),
                "skill '{}' has no origin metadata",
                skill.name
            );
        }
    }

    #[tokio::test]
    async fn origin_sources_are_known() {
        let skills = store().discover().await.unwrap();
        let sources: std::collections::HashSet<String> = skills
            .iter()
            .filter_map(|s| s.origin.as_ref()?.source.clone())
            .collect();
        assert_eq!(sources, std::collections::HashSet::from(["chelix".into()]));
    }

    #[tokio::test]
    async fn every_bundled_skill_body_is_readable() {
        let s = store();
        let skills = s.discover().await.unwrap();
        for skill in &skills {
            let body = s.read_skill(&skill.name).await;
            assert!(body.is_some(), "skill '{}' body not readable", skill.name);
            assert!(
                !body.as_ref().is_none_or(String::is_empty),
                "skill '{}' has empty body",
                skill.name
            );
        }
    }

    #[tokio::test]
    async fn missing_skill_returns_none() {
        assert!(store().read_skill("nonexistent-skill-xyz").await.is_none());
    }

    #[test]
    fn missing_sidecar_returns_none() {
        assert!(
            store()
                .read_sidecar("mcp-servers", "nonexistent.md")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn materialize_sidecars_writes_scripts_to_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let s = sidecar_fixture_store(tmp.path());

        let skill_dir = s
            .materialize_sidecars("sidecar-fixture")
            .unwrap()
            .expect("fixture should have sidecars to materialize");

        let script = skill_dir.join("scripts/fixture.sh");
        assert!(
            script.exists(),
            "fixture.sh must be materialized at {}",
            script.display()
        );
        let content = std::fs::read_to_string(&script).unwrap();
        assert_eq!(content, "#!/bin/sh\necho fixture\n");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&script).unwrap().permissions().mode();
            assert!(
                mode & 0o111 != 0,
                "scripts must be executable, got mode {mode:#o}"
            );
        }
    }

    #[test]
    fn materialize_sidecars_returns_none_for_skill_without_sidecars() {
        let s = store();
        let tmp = tempfile::tempdir().unwrap();

        assert!(s.list_sidecars("mcp-servers").unwrap().is_empty());
        assert!(
            s.materialize_sidecars_to("mcp-servers", tmp.path())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn materialize_sidecars_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let s = sidecar_fixture_store(tmp.path());

        let dir1 = s.materialize_sidecars("sidecar-fixture").unwrap();
        let dir2 = s.materialize_sidecars("sidecar-fixture").unwrap();
        assert_eq!(
            dir1, dir2,
            "repeated materialization must return the same path"
        );

        let script = dir2.unwrap().join("scripts/fixture.sh");
        assert!(script.exists());
    }

    #[tokio::test]
    async fn mcp_servers_is_chelix_native() {
        let s = store();
        let skills = store().discover().await.unwrap();
        let mcp_servers = skills
            .iter()
            .find(|skill| skill.name == "mcp-servers")
            .expect("mcp-servers should be bundled");
        assert_eq!(mcp_servers.category.as_deref(), Some("devops"));
        assert_eq!(
            mcp_servers
                .origin
                .as_ref()
                .and_then(|origin| origin.source.as_deref()),
            Some("chelix")
        );
        let body = s
            .read_skill("mcp-servers")
            .await
            .expect("body should exist");
        assert!(body.contains("`mcp_add`"));
        assert!(!body.contains("mcporter"));
    }
}
