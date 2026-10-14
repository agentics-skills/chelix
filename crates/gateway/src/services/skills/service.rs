use {async_trait::async_trait, serde_json::Value, std::sync::Arc};

use super::skills_helpers::*;

// Pull in ServiceResult, ServiceError, security_audit, etc. from services module
use super::super::*;

pub struct UnwiredSkillsService;

#[async_trait]
impl SkillsService for UnwiredSkillsService {
    async fn list(&self) -> ServiceResult {
        Err("skills service is not installed".into())
    }

    async fn skill_enable(&self, _params: Value) -> ServiceResult {
        Err("skills service is not installed".into())
    }

    async fn skill_disable(&self, _params: Value) -> ServiceResult {
        Err("skills service is not installed".into())
    }

    async fn skill_detail(&self, _params: Value) -> ServiceResult {
        Err("skills service is not installed".into())
    }

    async fn skill_save(&self, _params: Value) -> ServiceResult {
        Err("skills service is not installed".into())
    }

    async fn bundled_categories(&self) -> ServiceResult {
        Err("skills service is not installed".into())
    }

    async fn bundled_toggle_category(&self, _params: Value) -> ServiceResult {
        Err("skills service is not installed".into())
    }
}

pub struct NoopSkillsService {
    bus: Arc<chelix_call_bus::CallBus>,
}

impl NoopSkillsService {
    #[must_use]
    pub fn new(bus: Arc<chelix_call_bus::CallBus>) -> Self {
        Self { bus }
    }
}

#[async_trait]
impl SkillsService for NoopSkillsService {
    async fn list(&self) -> ServiceResult {
        use chelix_skills::discover::{FsSkillDiscoverer, SkillDiscoverer};
        let fs_discoverer =
            FsSkillDiscoverer::new(FsSkillDiscoverer::default_paths(), Arc::clone(&self.bus));

        #[cfg(feature = "bundled-skills")]
        let skills = {
            let bundled = Arc::new(chelix_skills::bundled::BundledSkillStore::new(Arc::clone(
                &self.bus,
            )));
            let composite = chelix_skills::discover::CompositeSkillDiscoverer::new(
                Box::new(fs_discoverer),
                bundled,
            );
            composite.discover().await.map_err(ServiceError::message)?
        };
        #[cfg(not(feature = "bundled-skills"))]
        let skills = fs_discoverer
            .discover()
            .await
            .map_err(ServiceError::message)?;
        let items: Vec<_> = skills
            .iter()
            .map(|s| {
                let protected = matches!(
                    s.source,
                    Some(SkillSource::Personal) | Some(SkillSource::Project)
                ) && is_protected_discovered_skill(&s.name);
                serde_json::json!({
                    "name": s.name,
                    "description": s.description,
                    "category": s.category,
                    "license": s.license,
                    "path": s.path.to_string_lossy(),
                    "source": s.source,
                    "protected": protected,
                })
            })
            .collect();
        Ok(serde_json::json!(items))
    }

    async fn skill_enable(&self, params: Value) -> ServiceResult {
        let source = params.get("source").and_then(|v| v.as_str()).unwrap_or("");
        if source == "bundled" {
            return toggle_bundled_skill(&self.bus, &params, true).await;
        }
        Err(format!("unsupported skill source '{source}'").into())
    }

    async fn skill_disable(&self, params: Value) -> ServiceResult {
        let source = params.get("source").and_then(|v| v.as_str()).unwrap_or("");

        // Personal/project skills live as files — delete the directory to disable.
        if source == "personal" || source == "project" {
            return delete_discovered_skill(source, &params);
        }

        if source == "bundled" {
            return toggle_bundled_skill(&self.bus, &params, false).await;
        }

        Err(format!("unsupported skill source '{source}'").into())
    }

    /// List bundled skill categories with skill counts and enabled state.
    async fn bundled_categories(&self) -> ServiceResult {
        #[cfg(feature = "bundled-skills")]
        {
            let store = chelix_skills::bundled::BundledSkillStore::new(Arc::clone(&self.bus));
            let skills = store.discover().await;
            let config = chelix_config::discover_and_load().map_err(ServiceError::message)?;
            let disabled = &config.skills.disabled_bundled_categories;

            let mut cats: std::collections::BTreeMap<String, u32> =
                std::collections::BTreeMap::new();
            for s in &skills {
                if let Some(cat) = &s.category {
                    *cats.entry(cat.clone()).or_insert(0) += 1;
                }
            }

            let categories: Vec<Value> = cats
                .into_iter()
                .map(|(name, count)| {
                    let enabled = !disabled.iter().any(|d| d == &name);
                    serde_json::json!({ "name": name, "count": count, "enabled": enabled })
                })
                .collect();

            Ok(serde_json::json!({ "categories": categories, "total_skills": skills.len() }))
        }
        #[cfg(not(feature = "bundled-skills"))]
        {
            Ok(serde_json::json!({ "categories": [], "total_skills": 0 }))
        }
    }

    /// Toggle a bundled skill category on or off.
    async fn bundled_toggle_category(&self, params: Value) -> ServiceResult {
        let category = params
            .get("category")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "missing 'category' parameter".to_string())?;
        let enabled = params
            .get("enabled")
            .and_then(|v| v.as_bool())
            .ok_or_else(|| "missing 'enabled' parameter".to_string())?;

        let category = category.to_string();
        let cat_clone = category.clone();

        if let Err(e) = chelix_config::update_config(|cfg| {
            if enabled {
                cfg.skills
                    .disabled_bundled_categories
                    .retain(|c| c != &cat_clone);
            } else if !cfg
                .skills
                .disabled_bundled_categories
                .iter()
                .any(|c| c == &cat_clone)
            {
                cfg.skills
                    .disabled_bundled_categories
                    .push(cat_clone.clone());
            }
        }) {
            return Err(format!("failed to save config: {e}").into());
        }

        Ok(serde_json::json!({ "category": category, "enabled": enabled }))
    }

    async fn skill_detail(&self, params: Value) -> ServiceResult {
        let source = params
            .get("source")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "missing 'source' parameter".to_string())?;
        let skill_name = params
            .get("skill")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "missing 'skill' parameter".to_string())?;

        // Personal/project skills: look up directly by name in discovered paths.
        if source == "personal" || source == "project" {
            return skill_detail_discovered(&self.bus, source, skill_name).await;
        }

        // Bundled skills: read from the embedded store.
        #[cfg(feature = "bundled-skills")]
        if source == "bundled" {
            return skill_detail_bundled(&self.bus, skill_name).await;
        }

        Err(format!("unsupported skill source '{source}'").into())
    }

    async fn skill_save(&self, params: Value) -> ServiceResult {
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "missing 'name' parameter".to_string())?;
        let description = params
            .get("description")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "missing 'description' parameter".to_string())?;
        let body = params
            .get("body")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "missing 'body' parameter".to_string())?;

        if !chelix_skills::parse::validate_name(name) {
            return Err(format!(
                "invalid skill name '{name}': must be 1-64 lowercase alphanumeric/hyphen chars"
            )
            .into());
        }

        let skills_dir = chelix_config::data_dir().join("skills");
        let skill_dir = skills_dir.join(name);
        let skill_md = skill_dir.join("SKILL.md");
        std::fs::create_dir_all(&skill_dir)
            .map_err(|e| format!("failed to create skill directory: {e}"))?;
        self.bus
            .call(ReplaceSkillFile {
                path: skill_md,
                name: name.to_string(),
                description: description.to_string(),
                body: body.to_string(),
            })
            .await
            .map_err(|error| format!("failed to write SKILL.md: {error}"))?;

        Ok(serde_json::json!({
            "saved": true,
            "name": name,
            "source": "personal",
            "path": skill_dir.to_string_lossy(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    struct SkillDataDirGuard {
        _config: ConfigDirGuard,
    }

    impl Drop for SkillDataDirGuard {
        fn drop(&mut self) {
            chelix_config::clear_data_dir();
        }
    }

    #[tokio::test]
    async fn skill_save_publishes_and_preserves_access_lists() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let config = ConfigDirGuard::new(dir.path().to_path_buf());
        chelix_config::set_data_dir(dir.path().to_path_buf());
        let _guard = SkillDataDirGuard { _config: config };
        let bus =
            chelix_skills::skill_file::open_skill_bus().map_err(|error| anyhow::anyhow!(error))?;
        let service = NoopSkillsService::new(Arc::clone(&bus));
        let params = serde_json::json!({
            "name": "demo",
            "description": "Use when: X\nNext step",
            "body": "body"
        });
        service.skill_save(params.clone()).await?;
        let path = dir.path().join("skills/demo/SKILL.md");
        let content = std::fs::read_to_string(&path)?;
        assert!(content.contains("allowed_agents:\n"));
        assert!(content.contains("denied_agents:\n"));
        let metadata = bus
            .call(ReadSkillFile {
                path: path.clone(),
                max_bytes: None,
            })
            .await?;
        assert_eq!(metadata.metadata.description, "Use when: X\nNext step");
        std::fs::write(
            &path,
            "---\nname: demo\ndescription: old\nallowed_agents:\n- agent1\ndenied_agents:\n---\nold body",
        )?;
        service.skill_save(params.clone()).await?;
        let saved = bus
            .call(ReadSkillFile {
                path: path.clone(),
                max_bytes: None,
            })
            .await?;
        assert_eq!(saved.metadata.allowed_agents, ["agent1"]);
        let invalid = "---\nname: demo\nallowed_agents: hello\n---\nbody";
        std::fs::write(&path, invalid)?;
        let error = service
            .skill_save(params)
            .await
            .err()
            .ok_or_else(|| anyhow::anyhow!("expected parse error"))?;
        assert!(error.to_string().contains("frontmatter"));
        assert_eq!(std::fs::read_to_string(path)?, invalid);
        Ok(())
    }

    struct ConfigDirGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl ConfigDirGuard {
        fn new(config_dir: PathBuf) -> Self {
            let lock = crate::config_override_test_lock();
            chelix_config::set_config_dir(config_dir);
            chelix_config::initialize_config()
                .unwrap_or_else(|error| panic!("test config should be initialized: {error}"));
            Self { _lock: lock }
        }
    }

    impl Drop for ConfigDirGuard {
        fn drop(&mut self) {
            chelix_config::clear_config_dir();
        }
    }

    #[cfg(feature = "bundled-skills")]
    #[tokio::test]
    async fn disabling_one_bundled_skill_does_not_disable_category() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let _guard = ConfigDirGuard::new(dir.path().to_path_buf());

        let bus =
            chelix_skills::skill_file::open_skill_bus().map_err(|error| anyhow::anyhow!(error))?;
        let service = NoopSkillsService::new(Arc::clone(&bus));
        let result = service
            .skill_disable(serde_json::json!({ "source": "bundled", "skill": "mcp-servers" }))
            .await?;

        assert_eq!(
            result.get("skill").and_then(Value::as_str),
            Some("mcp-servers")
        );

        let config = chelix_config::discover_and_load()?;
        assert!(config.skills.disabled_bundled_categories.is_empty());
        assert_eq!(config.skills.disabled_bundled_skills, vec![
            "mcp-servers".to_string()
        ]);
        assert!(
            !config
                .skills
                .is_bundled_skill_enabled("mcp-servers", Some("devops"))
        );
        Ok(())
    }

    #[cfg(feature = "bundled-skills")]
    #[tokio::test]
    async fn bundled_category_toggle_preserves_individual_disables() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let _guard = ConfigDirGuard::new(dir.path().to_path_buf());

        let bus =
            chelix_skills::skill_file::open_skill_bus().map_err(|error| anyhow::anyhow!(error))?;
        let service = NoopSkillsService::new(Arc::clone(&bus));
        service
            .skill_disable(serde_json::json!({ "source": "bundled", "skill": "mcp-servers" }))
            .await?;
        service
            .bundled_toggle_category(serde_json::json!({ "category": "devops", "enabled": false }))
            .await?;
        let category_enabled = service
            .bundled_toggle_category(serde_json::json!({ "category": "devops", "enabled": true }))
            .await?;
        assert_eq!(
            category_enabled.get("enabled").and_then(Value::as_bool),
            Some(true)
        );

        let config = chelix_config::discover_and_load()?;
        assert!(config.skills.disabled_bundled_categories.is_empty());
        assert_eq!(config.skills.disabled_bundled_skills, vec![
            "mcp-servers".to_string()
        ]);
        assert!(
            !config
                .skills
                .is_bundled_skill_enabled("mcp-servers", Some("devops"))
        );
        Ok(())
    }
}
