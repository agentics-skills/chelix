//! Create, update, and delete personal skills.

use {
    chelix_call_bus::{CallError, DeleteSkillDirectory, ReplaceDir, SkillActor, SkillFileError},
    std::sync::Arc,
};

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
    bus: Arc<chelix_call_bus::CallBus>,
    usage_store: Option<Arc<dyn SkillWriteRecorder>>,
}

impl CreateSkillTool {
    async fn create(&self, params: Value, id: &str) -> anyhow::Result<Value> {
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

        let content = self
            .bus
            .call(chelix_call_bus::CreateSkillFile {
                actor: SkillActor::Agent { id: id.into() },
                name: name.to_string(),
                description: description.to_string(),
                body: body.to_string(),
                creator_agent_id: Some(id.into()),
            })
            .await
            .map_err(|error| match error {
                CallError::Failed(SkillFileError::AlreadyExists(_)) => {
                    anyhow::anyhow!("skill '{name}' already exists; use update_skill to modify it")
                },
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
            "path": content.metadata.path.display().to_string(),
        }))
    }

    pub fn new(bus: Arc<chelix_call_bus::CallBus>) -> Self {
        Self {
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

    async fn execute(&self, _params: Value) -> anyhow::Result<Value> {
        Err(Error::message("missing agent id").into())
    }

    async fn execute_with_context(
        &self,
        params: Value,
        context: &ToolExecutionContext,
    ) -> anyhow::Result<Value> {
        self.create(params, super::helpers::agent_id(context)?)
            .await
    }
}

// ── UpdateSkillTool ─────────────────────────────────────────

/// Tool that updates an existing personal skill in `<data_dir>/skills/`.
pub struct UpdateSkillTool {
    bus: Arc<chelix_call_bus::CallBus>,
    usage_store: Option<Arc<dyn SkillWriteRecorder>>,
}

impl UpdateSkillTool {
    pub fn new(bus: Arc<chelix_call_bus::CallBus>) -> Self {
        Self {
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

    async fn execute(&self, _params: Value) -> anyhow::Result<Value> {
        Err(Error::message("missing agent id").into())
    }

    async fn execute_with_context(
        &self,
        params: Value,
        context: &ToolExecutionContext,
    ) -> anyhow::Result<Value> {
        let id = super::helpers::agent_id(context)?;
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

        let content = self
            .bus
            .call(chelix_call_bus::ReplaceSkillFile {
                actor: SkillActor::Agent { id: id.into() },
                dir: ReplaceDir::RequireExisting,
                name: name.to_string(),
                description: description.to_string(),
                body: body.to_string(),
            })
            .await
            .map_err(|error| super::helpers::missing_skill(name, error))?;

        if let Some(ref store) = self.usage_store {
            store.record_write(name).await;
        }
        #[cfg(feature = "metrics")]
        counter!(skills_metrics::MODIFICATIONS_TOTAL, labels::TOOL => "update_skill".to_string())
            .increment(1);

        Ok(json!({
            "updated": true,
            "path": content.metadata.path.display().to_string(),
        }))
    }
}

// ── DeleteSkillTool ─────────────────────────────────────────

/// Tool that deletes a personal skill from `<data_dir>/skills/`.
pub struct DeleteSkillTool {
    bus: Arc<chelix_call_bus::CallBus>,
    usage_store: Option<SkillUsageStore>,
}

impl DeleteSkillTool {
    pub fn new(bus: Arc<chelix_call_bus::CallBus>) -> Self {
        Self {
            bus,
            usage_store: None,
        }
    }

    #[must_use]
    pub fn with_usage_store(mut self, store: SkillUsageStore) -> Self {
        self.usage_store = Some(store);
        self
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

    async fn execute(&self, _params: Value) -> anyhow::Result<Value> {
        Err(Error::message("missing agent id").into())
    }

    async fn execute_with_context(
        &self,
        params: Value,
        context: &ToolExecutionContext,
    ) -> anyhow::Result<Value> {
        let id = super::helpers::agent_id(context)?;
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::message("missing 'name'"))?;

        self.bus
            .call(DeleteSkillDirectory {
                name: name.into(),
                agent_id: id.into(),
            })
            .await?;

        if let Some(ref store) = self.usage_store {
            store.remove(name).await;
        }

        Ok(json!({ "deleted": true }))
    }
}
