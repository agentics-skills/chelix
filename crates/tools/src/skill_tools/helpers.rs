use {
    crate::error::Error,
    chelix_agents::tool_context::ToolExecutionContext,
    chelix_call_bus::{CallError, SkillFileError},
};

pub(super) fn agent_id(context: &ToolExecutionContext) -> crate::Result<&str> {
    context
        .sender_agent_id()
        .ok_or_else(|| Error::message("missing agent id"))
}

pub(super) fn missing_skill(name: &str, error: CallError<SkillFileError>) -> anyhow::Error {
    match error {
        CallError::Failed(SkillFileError::NotFound(_)) => {
            anyhow::anyhow!("skill '{name}' does not exist; use create_skill first")
        },
        other => anyhow::anyhow!(other),
    }
}

#[async_trait::async_trait]
pub(super) trait SkillWriteRecorder: Send + Sync {
    async fn record_write(&self, name: &str);
}

#[async_trait::async_trait]
impl SkillWriteRecorder for chelix_skills::usage::SkillUsageStore {
    async fn record_write(&self, name: &str) {
        Self::record_write(self, name).await;
    }
}
