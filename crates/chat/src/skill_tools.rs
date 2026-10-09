use std::{path::PathBuf, sync::Arc};

use {
    async_trait::async_trait,
    chelix_agents::{
        tool_context::ToolExecutionContext,
        tool_registry::{AgentTool, ToolRegistry, ToolResultPersistence, Truncation},
    },
    chelix_config::{ChelixConfig, schema::AgentSkillPolicy},
    serde_json::Value,
};

use crate::prompt::{discover_skills_if_enabled, filter_skills_for_agent};

#[derive(Clone, Copy)]
enum SkillToolKind {
    Read,
    Create,
    Modify,
    Delete,
}

struct AgentScopedSkillTool {
    inner: Arc<dyn AgentTool>,
    kind: SkillToolKind,
    agent_id: String,
    policy: AgentSkillPolicy,
    config: Arc<ChelixConfig>,
    data_dir: PathBuf,
}

impl AgentScopedSkillTool {
    async fn authorize(&self, params: &Value) -> anyhow::Result<()> {
        let Some(name) = params.get("name").and_then(Value::as_str) else {
            return Ok(());
        };
        if matches!(self.kind, SkillToolKind::Read) {
            let visible = filter_skills_for_agent(
                discover_skills_if_enabled(&self.config).await,
                &self.agent_id,
                &self.policy,
            );
            if visible.iter().any(|skill| skill.name == name) {
                return Ok(());
            }
            let hint = if visible.is_empty() {
                "no skills are currently available".to_string()
            } else {
                format!(
                    "available skills: {}",
                    visible
                        .iter()
                        .map(|skill| skill.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            anyhow::bail!(
                "skill '{name}' not found ({hint}). Use one of the names listed in <available_skills>."
            );
        }
        if !chelix_skills::parse::validate_name(name) {
            return Ok(());
        }
        let skill_dir = self.data_dir.join("skills").join(name);
        let content = match tokio::fs::read_to_string(skill_dir.join("SKILL.md")).await {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let skill = chelix_skills::parse::parse_metadata(&content, &skill_dir)?;
        if chelix_skills::visible_to_agent(&self.agent_id, &skill, &self.policy) {
            return Ok(());
        }
        if matches!(self.kind, SkillToolKind::Create | SkillToolKind::Delete) {
            anyhow::bail!("skill '{name}' not found");
        }
        anyhow::bail!("skill '{name}' does not exist; use create_skill first");
    }
}

#[async_trait]
impl AgentTool for AgentScopedSkillTool {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn description(&self) -> &str {
        self.inner.description()
    }

    fn parameters_schema(&self) -> Value {
        self.inner.parameters_schema()
    }

    fn parameters_schema_with_max_tool_result_bytes(&self, bytes: usize) -> Value {
        self.inner
            .parameters_schema_with_max_tool_result_bytes(bytes)
    }

    fn validate(&self, params: &Value) -> anyhow::Result<()> {
        self.inner.validate(params)
    }

    fn ui_presentation(
        &self,
        lifecycle: &chelix_common::tool_lifecycle::ToolLifecycleEvent,
    ) -> anyhow::Result<Option<chelix_sessions::ui_history_types::UiPresentation>> {
        self.inner.ui_presentation(lifecycle)
    }

    async fn warmup(&self) -> anyhow::Result<()> {
        self.inner.warmup().await
    }

    fn truncation(&self, params: &Value) -> Truncation {
        self.inner.truncation(params)
    }

    fn result_persistence(&self, params: &Value) -> ToolResultPersistence {
        self.inner.result_persistence(params)
    }

    fn in_context_result_bytes(&self, params: &Value) -> Option<usize> {
        self.inner.in_context_result_bytes(params)
    }

    async fn agent_result(&self, params: &Value, raw_result: &Value) -> anyhow::Result<Value> {
        self.inner.agent_result(params, raw_result).await
    }

    async fn execute(&self, params: Value) -> anyhow::Result<Value> {
        self.authorize(&params).await?;
        self.inner.execute(params).await
    }

    async fn execute_with_context(
        &self,
        params: Value,
        context: &ToolExecutionContext,
    ) -> anyhow::Result<Value> {
        self.authorize(&params).await?;
        self.inner.execute_with_context(params, context).await
    }
}

pub(crate) fn install_agent_scoped_skill_tools(
    registry: &mut ToolRegistry,
    config: &ChelixConfig,
    agent_id: &str,
) -> anyhow::Result<()> {
    let config = Arc::new(config.clone());
    let policy = config
        .agents
        .get(agent_id)
        .ok_or_else(|| anyhow::anyhow!("unknown agent '{agent_id}'"))?
        .skills
        .clone();
    for (name, kind) in [
        ("read_skill", SkillToolKind::Read),
        ("patch_skill", SkillToolKind::Modify),
        ("create_skill", SkillToolKind::Create),
        ("update_skill", SkillToolKind::Modify),
        ("delete_skill", SkillToolKind::Delete),
        ("write_skill_files", SkillToolKind::Modify),
    ] {
        if let Some(inner) = registry.get(name) {
            registry.replace(Box::new(AgentScopedSkillTool {
                inner,
                kind,
                agent_id: agent_id.to_string(),
                policy: policy.clone(),
                config: Arc::clone(&config),
                data_dir: chelix_config::data_dir(),
            }));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        serde_json::json,
        std::sync::atomic::{AtomicUsize, Ordering},
    };

    struct DataDirGuard;

    impl Drop for DataDirGuard {
        fn drop(&mut self) {
            chelix_config::clear_data_dir();
        }
    }

    struct RecordingTool {
        name: &'static str,
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl AgentTool for RecordingTool {
        fn name(&self) -> &str {
            self.name
        }

        fn description(&self) -> &str {
            "Record delegation"
        }

        fn parameters_schema(&self) -> Value {
            json!({ "type": "object", "properties": { "name": { "type": "string" } } })
        }

        async fn execute(&self, params: Value) -> anyhow::Result<Value> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(json!({ "params": params, "context": false }))
        }

        async fn execute_with_context(
            &self,
            params: Value,
            context: &ToolExecutionContext,
        ) -> anyhow::Result<Value> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(json!({ "params": params, "context": true, "sender": context.sender_agent_id() }))
        }
    }

    #[tokio::test]
    async fn mutation_wrappers_gate_personal_files_and_delegate_context() -> anyhow::Result<()> {
        let _lock = crate::DATA_DIR_TEST_LOCK.lock().await;
        let dir = tempfile::tempdir()?;
        let _guard = DataDirGuard;
        chelix_config::set_data_dir(dir.path().to_path_buf());
        let personal = dir.path().join("skills/demo");
        let project = dir.path().join(".chelix/skills/demo");
        std::fs::create_dir_all(&personal)?;
        std::fs::create_dir_all(&project)?;
        std::fs::write(
            project.join("SKILL.md"),
            "---\nname: demo\ndeny: [agent1]\n---\nproject",
        )?;
        let path = personal.join("SKILL.md");
        let context = ToolExecutionContext::for_session_with_agent(
            chelix_sessions::SessionKey::new("session:test"),
            "agent1",
        );
        let config = Arc::new(ChelixConfig::default());
        for (name, kind, hidden_error) in [
            (
                "create_skill",
                SkillToolKind::Create,
                "skill 'demo' not found",
            ),
            (
                "update_skill",
                SkillToolKind::Modify,
                "skill 'demo' does not exist; use create_skill first",
            ),
            (
                "patch_skill",
                SkillToolKind::Modify,
                "skill 'demo' does not exist; use create_skill first",
            ),
            (
                "delete_skill",
                SkillToolKind::Delete,
                "skill 'demo' not found",
            ),
            (
                "write_skill_files",
                SkillToolKind::Modify,
                "skill 'demo' does not exist; use create_skill first",
            ),
        ] {
            let calls = Arc::new(AtomicUsize::new(0));
            let inner = Arc::new(RecordingTool {
                name,
                calls: Arc::clone(&calls),
            });
            let schema = inner.parameters_schema();
            let wrapper = AgentScopedSkillTool {
                inner,
                kind,
                agent_id: "agent1".into(),
                policy: AgentSkillPolicy::default(),
                config: Arc::clone(&config),
                data_dir: dir.path().to_path_buf(),
            };
            assert_eq!(wrapper.parameters_schema(), schema);
            let invalid = "---\nname: demo\nallow:\n---\nbody";
            std::fs::write(&path, invalid)?;
            let error = wrapper
                .execute_with_context(json!({ "name": "demo" }), &context)
                .await
                .err()
                .ok_or_else(|| anyhow::anyhow!("parse error"))?;
            assert!(error.to_string().contains("frontmatter"));
            assert_eq!(std::fs::read_to_string(&path)?, invalid);
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            for declaration in ["deny: [agent1]", "allow: [agent2]"] {
                std::fs::write(&path, format!("---\nname: demo\n{declaration}\n---\nbody"))?;
                let error = wrapper
                    .execute_with_context(json!({ "name": "demo" }), &context)
                    .await
                    .err()
                    .ok_or_else(|| anyhow::anyhow!("hidden skill"))?;
                assert_eq!(error.to_string(), hidden_error);
                assert_eq!(calls.load(Ordering::SeqCst), 0);
            }
            for params in [json!({}), json!({ "name": "Bad Name" })] {
                let delegated = wrapper
                    .execute_with_context(params.clone(), &context)
                    .await?;
                assert_eq!(delegated["params"], params);
                assert_eq!(delegated["sender"], "agent1");
            }
            std::fs::write(&path, "---\nname: demo\nallow: [agent1]\n---\nbody")?;
            let visible = wrapper
                .execute_with_context(json!({ "name": "demo" }), &context)
                .await?;
            assert_eq!(visible["context"], true);
            std::fs::remove_file(&path)?;
            let missing = wrapper
                .execute_with_context(json!({ "name": "demo" }), &context)
                .await?;
            assert_eq!(missing["context"], true);
            assert_eq!(calls.load(Ordering::SeqCst), 4);
        }
        Ok(())
    }

    #[tokio::test]
    async fn read_wrapper_refreshes_discovery_and_hides_first_source_and_hint() -> anyhow::Result<()>
    {
        let _lock = crate::DATA_DIR_TEST_LOCK.lock().await;
        let dir = tempfile::tempdir()?;
        let _guard = DataDirGuard;
        chelix_config::set_data_dir(dir.path().to_path_buf());
        let project = dir.path().join(".chelix/skills/demo");
        let personal = dir.path().join("skills/demo");
        std::fs::create_dir_all(&project)?;
        std::fs::create_dir_all(&personal)?;
        std::fs::write(
            personal.join("SKILL.md"),
            "---\nname: demo\nallow: [agent1]\n---\npersonal",
        )?;
        let path = project.join("SKILL.md");
        std::fs::write(&path, "---\nname: demo\ndeny: [agent1]\n---\nproject")?;
        let plugin = dir.path().join("installed-plugins/demo");
        std::fs::create_dir_all(plugin.join("commands"))?;
        std::fs::write(
            plugin.join("commands/review.md"),
            "---\ndescription: Review\ndeny: [agent1]\n---\ncommand body",
        )?;
        std::fs::write(
            plugin.join("SKILL.md"),
            "---\nname: demo\ndeny: [agent1]\n---\nplugin body",
        )?;
        let manifest = chelix_skills::types::SkillsManifest {
            version: 1,
            repos: vec![chelix_skills::types::RepoEntry {
                source: "demo/plugin".into(),
                repo_name: "demo".into(),
                installed_at_ms: 0,
                commit_sha: None,
                format: chelix_skills::formats::PluginFormat::ClaudeCode,
                quarantined: false,
                quarantine_reason: None,
                provenance: None,
                skills: vec![
                    chelix_skills::types::SkillState {
                        name: "Demo:pdf".into(),
                        relative_path: "demo/commands/review.md".into(),
                        trusted: true,
                        enabled: true,
                    },
                    chelix_skills::types::SkillState {
                        name: "claude-dir".into(),
                        relative_path: "demo".into(),
                        trusted: true,
                        enabled: true,
                    },
                ],
            }],
        };
        chelix_skills::manifest::ManifestStore::new(dir.path().join("plugins-manifest.json"))
            .save(&manifest)?;
        let context = ToolExecutionContext::for_session_with_agent(
            chelix_sessions::SessionKey::new("session:test"),
            "agent1",
        );
        let discoverer = Arc::new(chelix_skills::discover::FsSkillDiscoverer::new(
            chelix_skills::discover::FsSkillDiscoverer::default_paths_for(dir.path()),
        ));
        let inner: Arc<dyn AgentTool> =
            Arc::new(chelix_tools::skill_tools::ReadSkillTool::new(discoverer));
        let schema = inner.parameters_schema();
        let mut config = ChelixConfig::default();
        config.agents.entries.insert(
            "agent1".into(),
            chelix_config::AgentConfig::new(
                "Agent",
                "test::model",
                chelix_config::schema::ReasoningEffort::from("off"),
            ),
        );
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(
            chelix_tools::skill_tools::ReadSkillTool::with_default_paths(),
        ));
        install_agent_scoped_skill_tools(&mut registry, &config, "agent1")?;
        assert_eq!(registry.list_names(), ["read_skill"]);
        let installed = registry
            .get("read_skill")
            .ok_or_else(|| anyhow::anyhow!("read tool"))?;
        assert_eq!(installed.parameters_schema(), schema);
        let missing = installed
            .execute_with_context(json!({}), &context)
            .await
            .err()
            .ok_or_else(|| anyhow::anyhow!("missing name"))?;
        assert_eq!(missing.to_string(), "missing 'name'");
        for name in ["demo", "", "Demo:pdf"] {
            let error = installed
                .execute_with_context(json!({ "name": name }), &context)
                .await
                .err()
                .ok_or_else(|| anyhow::anyhow!("hidden read"))?;
            let message = error.to_string();
            assert!(message.starts_with(&format!("skill '{name}' not found (")));
            assert!(!message.contains("available skills: demo"));
            assert!(!message.contains(", demo"));
            let hint = message
                .split_once("not found (")
                .ok_or_else(|| anyhow::anyhow!("hint"))?
                .1;
            assert!(!hint.contains("Demo:pdf"));
            assert!(!hint.contains("claude-dir"));
        }
        std::fs::write(&path, "---\nname: demo\nallow: [agent1]\n---\nproject")?;
        let response = installed
            .execute_with_context(json!({ "name": "demo" }), &context)
            .await?;
        let original = inner
            .execute_with_context(json!({ "name": "demo" }), &context)
            .await?;
        assert_eq!(response, original);
        assert_eq!(response["body"], "project");
        std::fs::write(&path, "---\nname: demo\ndeny: [agent1]\n---\nproject")?;
        assert!(
            installed
                .execute_with_context(json!({ "name": "demo" }), &context)
                .await
                .is_err()
        );
        Ok(())
    }
}
