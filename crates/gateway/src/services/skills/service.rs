use {
    super::{super::*, skills_helpers::*},
    async_trait::async_trait,
    chelix_call_bus::{
        ListSkills, ReplaceDir, ReplaceSkillFile, SetBundledCategoryEnabled, SkillActor,
        SkillSource, SkillView,
    },
    serde_json::Value,
    std::sync::Arc,
};

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
        let skills = self
            .bus
            .call(ListSkills::Process {
                view: SkillView::Merged,
            })
            .await
            .map_err(|error| ServiceError::message(error.to_string()))?;
        Ok(serde_json::json!(skills.into_iter().map(|skill| {
            let s = skill.metadata;
            let protected = matches!(s.source, Some(SkillSource::Personal) | Some(SkillSource::Project)) && is_protected_discovered_skill(&s.name);
            serde_json::json!({ "name": s.name, "description": s.description, "category": s.category, "license": s.license, "path": s.path.to_string_lossy(), "source": s.source, "protected": protected })
        }).collect::<Vec<_>>()))
    }

    async fn skill_enable(&self, params: Value) -> ServiceResult {
        let source = params.get("source").and_then(Value::as_str).unwrap_or("");
        if source == "bundled" {
            return toggle_bundled_skill(&self.bus, &params, true).await;
        }
        Err(format!("unsupported skill source '{source}'").into())
    }

    async fn skill_disable(&self, params: Value) -> ServiceResult {
        let source = params.get("source").and_then(Value::as_str).unwrap_or("");
        if source == "personal" || source == "project" {
            return delete_discovered_skill(source, &params);
        }
        if source == "bundled" {
            return toggle_bundled_skill(&self.bus, &params, false).await;
        }
        Err(format!("unsupported skill source '{source}'").into())
    }

    async fn bundled_categories(&self) -> ServiceResult {
        let skills = self
            .bus
            .call(ListSkills::Process {
                view: SkillView::Bundled,
            })
            .await
            .map_err(|error| ServiceError::message(error.to_string()))?;
        let mut cats = std::collections::BTreeMap::<String, (u32, bool)>::new();
        for skill in &skills {
            if let Some(category) = &skill.metadata.category {
                let entry = cats
                    .entry(category.clone())
                    .or_insert((0, skill.category_enabled));
                entry.0 += 1;
            }
        }
        let categories = cats.into_iter().map(|(name, (count, enabled))| serde_json::json!({ "name": name, "count": count, "enabled": enabled })).collect::<Vec<_>>();
        Ok(serde_json::json!({ "categories": categories, "total_skills": skills.len() }))
    }

    async fn bundled_toggle_category(&self, params: Value) -> ServiceResult {
        let category = params
            .get("category")
            .and_then(Value::as_str)
            .ok_or_else(|| "missing 'category' parameter".to_string())?;
        let enabled = params
            .get("enabled")
            .and_then(Value::as_bool)
            .ok_or_else(|| "missing 'enabled' parameter".to_string())?;
        let result = self
            .bus
            .call(SetBundledCategoryEnabled {
                category: category.into(),
                enabled,
            })
            .await
            .map_err(|error| ServiceError::message(error.to_string()))?;
        Ok(serde_json::json!({ "category": result.category, "enabled": result.enabled }))
    }

    async fn skill_detail(&self, params: Value) -> ServiceResult {
        let source = params
            .get("source")
            .and_then(Value::as_str)
            .ok_or_else(|| "missing 'source' parameter".to_string())?;
        let name = params
            .get("skill")
            .and_then(Value::as_str)
            .ok_or_else(|| "missing 'skill' parameter".to_string())?;
        if source == "personal" || source == "project" {
            return skill_detail_discovered(&self.bus, source, name).await;
        }
        if source == "bundled" {
            return skill_detail_bundled(&self.bus, name).await;
        }
        Err(format!("unsupported skill source '{source}'").into())
    }

    async fn skill_save(&self, params: Value) -> ServiceResult {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| "missing 'name' parameter".to_string())?;
        let description = params
            .get("description")
            .and_then(Value::as_str)
            .ok_or_else(|| "missing 'description' parameter".to_string())?;
        let body = params
            .get("body")
            .and_then(Value::as_str)
            .ok_or_else(|| "missing 'body' parameter".to_string())?;
        let content = self
            .bus
            .call(ReplaceSkillFile {
                name: name.into(),
                description: description.into(),
                body: body.into(),
                actor: SkillActor::Process,
                dir: ReplaceDir::Create,
            })
            .await
            .map_err(|error| format!("failed to write SKILL.md: {error}"))?;
        Ok(
            serde_json::json!({ "saved": true, "name": name, "source": "personal", "path": content.metadata.path.to_string_lossy() }),
        )
    }
}
#[cfg(test)]
mod tests {
    use {
        chelix_call_bus::{ReadSkillFile, SkillLocate},
        std::path::PathBuf,
    };

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
        let bus = chelix_skills::skill_file::open_skill_bus_at(dir.path())
            .map_err(|error| anyhow::anyhow!(error))?;
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
                name: "demo".into(),
                actor: SkillActor::Process,
                locate: SkillLocate::PersonalDir,
                file_path: None,
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
                name: "demo".into(),
                actor: SkillActor::Process,
                locate: SkillLocate::PersonalDir,
                file_path: None,
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
}
