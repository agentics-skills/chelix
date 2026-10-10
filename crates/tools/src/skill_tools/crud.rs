//! Create, update, and delete personal skills.

use std::{path::PathBuf, sync::Arc};

use {
    async_trait::async_trait,
    chelix_agents::{tool_context::ToolExecutionContext, tool_registry::AgentTool},
    chelix_skills::usage::SkillUsageStore,
    serde_json::{Value, json},
};

#[cfg(feature = "metrics")]
use chelix_metrics::{counter, labels, skills as skills_metrics};

use {super::helpers::SkillWriteRecorder, crate::error::Error};

// ── CreateSkillTool ─────────────────────────────────────────

/// Tool that creates a new personal skill in `<data_dir>/skills/`.
pub struct CreateSkillTool {
    data_dir: PathBuf,
    bus: Arc<chelix_call_bus::CallBus>,
    usage_store: Option<Arc<dyn SkillWriteRecorder>>,
}

impl CreateSkillTool {
    async fn create(&self, params: Value, on_create: Option<&str>) -> anyhow::Result<Value> {
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::message("missing 'name'"))?;
        let description = params
            .get("description")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::message("missing 'description'"))?;
        let body = params
            .get("body")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::message("missing 'body'"))?;

        if !chelix_skills::parse::validate_name(name) {
            return Err(Error::message(format!(
                "invalid skill name '{name}': must be 1-64 lowercase alphanumeric/hyphen chars"
            ))
            .into());
        }

        let skill_dir = self.skills_dir().join(name);
        if skill_dir.exists() {
            return Err(Error::message(format!(
                "skill '{name}' already exists; use update_skill to modify it"
            ))
            .into());
        }
        tokio::fs::create_dir_all(&skill_dir).await?;
        self.bus
            .call(chelix_service_traits::CreateSkillFile {
                path: skill_dir.join("SKILL.md"),
                name: name.to_string(),
                description: description.to_string(),
                body: body.to_string(),
                creator_agent_id: on_create.map(str::to_string),
            })
            .await
            .map_err(|error| anyhow::anyhow!(error))?;

        if let Some(ref store) = self.usage_store {
            store.record_write(name).await;
        }
        #[cfg(feature = "metrics")]
        counter!(skills_metrics::MODIFICATIONS_TOTAL, labels::TOOL => "create_skill".to_string())
            .increment(1);

        Ok(json!({
            "created": true,
            "path": skill_dir.display().to_string(),
        }))
    }

    pub fn new(data_dir: PathBuf, bus: Arc<chelix_call_bus::CallBus>) -> Self {
        Self {
            data_dir,
            bus,
            usage_store: None,
        }
    }

    #[must_use]
    pub fn with_usage_store(mut self, store: SkillUsageStore) -> Self {
        self.usage_store = Some(Arc::new(store));
        self
    }

    #[cfg(test)]
    pub(super) fn with_write_recorder(mut self, recorder: Arc<dyn SkillWriteRecorder>) -> Self {
        self.usage_store = Some(recorder);
        self
    }

    fn skills_dir(&self) -> PathBuf {
        self.data_dir.join("skills")
    }
}

#[async_trait]
impl AgentTool for CreateSkillTool {
    fn name(&self) -> &str {
        "create_skill"
    }

    fn description(&self) -> &str {
        "Create a new personal skill. Writes a SKILL.md file to <data_dir>/skills/<name>/. \
         This is persistent workspace storage (not sandbox ~/skills). \
         The skill will be available on the next message automatically."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["name", "description", "body"],
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Skill name (lowercase, hyphens, 1-64 chars)"
                },
                "description": {
                    "type": "string",
                    "description": "Short human-readable description"
                },
                "body": {
                    "type": "string",
                    "description": "Markdown instructions for the skill"
                }
            }
        })
    }

    async fn execute(&self, params: Value) -> anyhow::Result<Value> {
        self.create(params, None).await
    }

    async fn execute_with_context(
        &self,
        params: Value,
        context: &ToolExecutionContext,
    ) -> anyhow::Result<Value> {
        self.create(params, context.sender_agent_id()).await
    }
}

// ── UpdateSkillTool ─────────────────────────────────────────

/// Tool that updates an existing personal skill in `<data_dir>/skills/`.
pub struct UpdateSkillTool {
    data_dir: PathBuf,
    bus: Arc<chelix_call_bus::CallBus>,
    usage_store: Option<Arc<dyn SkillWriteRecorder>>,
}

impl UpdateSkillTool {
    pub fn new(data_dir: PathBuf, bus: Arc<chelix_call_bus::CallBus>) -> Self {
        Self {
            data_dir,
            bus,
            usage_store: None,
        }
    }

    #[must_use]
    pub fn with_usage_store(mut self, store: SkillUsageStore) -> Self {
        self.usage_store = Some(Arc::new(store));
        self
    }

    #[cfg(test)]
    pub(super) fn with_write_recorder(mut self, recorder: Arc<dyn SkillWriteRecorder>) -> Self {
        self.usage_store = Some(recorder);
        self
    }

    fn skills_dir(&self) -> PathBuf {
        self.data_dir.join("skills")
    }
}

#[async_trait]
impl AgentTool for UpdateSkillTool {
    fn name(&self) -> &str {
        "update_skill"
    }

    fn description(&self) -> &str {
        "Update an existing personal skill. Overwrites the SKILL.md file."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["name", "description", "body"],
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Skill name to update"
                },
                "description": {
                    "type": "string",
                    "description": "New short description"
                },
                "body": {
                    "type": "string",
                    "description": "New markdown instructions"
                }
            }
        })
    }

    async fn execute(&self, params: Value) -> anyhow::Result<Value> {
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::message("missing 'name'"))?;
        let description = params
            .get("description")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::message("missing 'description'"))?;
        let body = params
            .get("body")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::message("missing 'body'"))?;

        if !chelix_skills::parse::validate_name(name) {
            return Err(Error::message(format!(
                "invalid skill name '{name}': must be 1-64 lowercase alphanumeric/hyphen chars"
            ))
            .into());
        }

        let skill_dir = self.skills_dir().join(name);
        if !skill_dir.exists() {
            return Err(Error::message(format!(
                "skill '{name}' does not exist; use create_skill first"
            ))
            .into());
        }

        self.bus
            .call(chelix_service_traits::ReplaceSkillFile {
                path: skill_dir.join("SKILL.md"),
                name: name.to_string(),
                description: description.to_string(),
                body: body.to_string(),
            })
            .await
            .map_err(|error| anyhow::anyhow!(error))?;

        if let Some(ref store) = self.usage_store {
            store.record_write(name).await;
        }
        #[cfg(feature = "metrics")]
        counter!(skills_metrics::MODIFICATIONS_TOTAL, labels::TOOL => "update_skill".to_string())
            .increment(1);

        Ok(json!({
            "updated": true,
            "path": skill_dir.display().to_string(),
        }))
    }
}

// ── DeleteSkillTool ─────────────────────────────────────────

/// Tool that deletes a personal skill from `<data_dir>/skills/`.
pub struct DeleteSkillTool {
    data_dir: PathBuf,
    bus: Arc<chelix_call_bus::CallBus>,
    usage_store: Option<SkillUsageStore>,
}

impl DeleteSkillTool {
    pub fn new(data_dir: PathBuf, bus: Arc<chelix_call_bus::CallBus>) -> Self {
        Self {
            data_dir,
            bus,
            usage_store: None,
        }
    }

    #[must_use]
    pub fn with_usage_store(mut self, store: SkillUsageStore) -> Self {
        self.usage_store = Some(store);
        self
    }

    fn skills_dir(&self) -> PathBuf {
        self.data_dir.join("skills")
    }
}

#[async_trait]
impl AgentTool for DeleteSkillTool {
    fn name(&self) -> &str {
        "delete_skill"
    }

    fn description(&self) -> &str {
        "Delete a personal skill. Removes the full skill directory, including supplementary files."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["name"],
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Skill name to delete"
                }
            }
        })
    }

    async fn execute(&self, params: Value) -> anyhow::Result<Value> {
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::message("missing 'name'"))?;

        if !chelix_skills::parse::validate_name(name) {
            return Err(Error::message(format!("invalid skill name '{name}'")).into());
        }

        let skill_dir = self.skills_dir().join(name);
        if !skill_dir.exists() {
            return Err(Error::message(format!("skill '{name}' not found")).into());
        }

        match self
            .bus
            .call(chelix_service_traits::ReadSkillFile {
                path: skill_dir.join("SKILL.md"),
                max_bytes: None,
                mode: chelix_service_traits::SkillFileRead::Detect,
            })
            .await
        {
            Ok(_) => {},
            Err(chelix_call_bus::CallError::Failed(
                chelix_service_traits::SkillFileError::NotFound(_),
            )) => {},
            Err(error) => return Err(error.into()),
        }
        tokio::fs::remove_dir_all(&skill_dir).await?;

        if let Some(ref store) = self.usage_store {
            store.remove(name).await;
        }

        Ok(json!({ "deleted": true }))
    }
}
