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
    files: Arc<chelix_skills::skill_file::SkillFiles>,
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
        chelix_skills::skill_file::create(
            &self.files,
            &skill_dir.join("SKILL.md"),
            chelix_skills::skill_file::SkillFields {
                name: name.to_string(),
                description: description.to_string(),
                body: body.to_string(),
            },
            on_create,
        )
        .await
        .map_err(|error| match error {
            chelix_skills::Error::AlreadyExists(message) => anyhow::anyhow!(message),
            other => anyhow::anyhow!(other),
        })?;

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

    pub fn new(data_dir: PathBuf, files: Arc<chelix_skills::skill_file::SkillFiles>) -> Self {
        Self {
            data_dir,
            files,
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
    files: Arc<chelix_skills::skill_file::SkillFiles>,
    usage_store: Option<Arc<dyn SkillWriteRecorder>>,
}

impl UpdateSkillTool {
    pub fn new(data_dir: PathBuf, files: Arc<chelix_skills::skill_file::SkillFiles>) -> Self {
        Self {
            data_dir,
            files,
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

        chelix_skills::skill_file::replace(
            &self.files,
            &skill_dir.join("SKILL.md"),
            chelix_skills::skill_file::SkillFields {
                name: name.to_string(),
                description: description.to_string(),
                body: body.to_string(),
            },
        )
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
    files: Arc<chelix_skills::skill_file::SkillFiles>,
    usage_store: Option<SkillUsageStore>,
}

impl DeleteSkillTool {
    pub fn new(data_dir: PathBuf, files: Arc<chelix_skills::skill_file::SkillFiles>) -> Self {
        Self {
            data_dir,
            files,
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

        match chelix_skills::skill_file::read(&self.files, &skill_dir.join("SKILL.md"), None).await
        {
            Ok(_) => {},
            Err(chelix_skills::Error::NotFound(_)) => {},
            Err(chelix_skills::Error::Io(io)) if io.kind() == std::io::ErrorKind::NotFound => {},
            Err(error) => return Err(error.into()),
        }
        tokio::fs::remove_dir_all(&skill_dir).await?;

        if let Some(ref store) = self.usage_store {
            store.remove(name).await;
        }

        Ok(json!({ "deleted": true }))
    }
}
