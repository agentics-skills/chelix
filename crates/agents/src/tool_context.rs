use chelix_sessions::SessionKey;

/// Trusted execution context supplied by the agent runner separately from a
/// tool's public JSON arguments.
#[derive(Debug, Clone)]
pub struct ToolExecutionContext {
    session_key: Option<SessionKey>,
    agent_id: Option<String>,
    execution_arguments: Option<serde_json::Value>,
}

impl ToolExecutionContext {
    /// Build execution context for a direct, session-scoped tool invocation.
    #[must_use]
    pub fn for_session(session_key: SessionKey) -> Self {
        Self {
            session_key: Some(session_key),
            agent_id: None,
            execution_arguments: None,
        }
    }

    /// Build execution context with an explicit trusted sender agent.
    #[must_use]
    pub fn for_session_with_agent(session_key: SessionKey, agent_id: impl Into<String>) -> Self {
        Self {
            session_key: Some(session_key),
            agent_id: Some(agent_id.into()),
            execution_arguments: None,
        }
    }

    /// Return the canonical session key when this run has session context.
    #[must_use]
    pub const fn session_key(&self) -> Option<&SessionKey> {
        self.session_key.as_ref()
    }

    /// Return the trusted sender agent id for the running agent.
    #[must_use]
    pub fn sender_agent_id(&self) -> Option<&str> {
        self.agent_id.as_deref()
    }

    /// Require canonical session context for a session-only tool.
    pub fn require_session_key(&self) -> anyhow::Result<&SessionKey> {
        self.session_key
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("session execution context is required"))
    }

    pub(crate) fn from_runner(
        trusted_context: Option<&serde_json::Value>,
        execution_arguments: serde_json::Value,
    ) -> Self {
        let session_key = trusted_context
            .and_then(serde_json::Value::as_object)
            .and_then(|context| context.get("_session_key"))
            .and_then(serde_json::Value::as_str)
            .filter(|session_key| !session_key.is_empty())
            .map(SessionKey::new);
        let agent_id = trusted_context
            .and_then(serde_json::Value::as_object)
            .and_then(|context| context.get("_agent_id"))
            .and_then(serde_json::Value::as_str)
            .filter(|agent_id| !agent_id.trim().is_empty())
            .map(str::to_string);
        Self {
            session_key,
            agent_id,
            execution_arguments: Some(execution_arguments),
        }
    }

    pub(crate) const fn execution_arguments(&self) -> Option<&serde_json::Value> {
        self.execution_arguments.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_runner_extracts_trusted_agent_id() {
        let trusted = serde_json::json!({
            "_session_key": "session:parent",
            "_agent_id": "coder",
        });
        let context = ToolExecutionContext::from_runner(Some(&trusted), serde_json::json!({}));
        assert_eq!(
            context.session_key().map(|key| key.as_str()),
            Some("session:parent")
        );
        assert_eq!(context.sender_agent_id(), Some("coder"));
    }

    #[test]
    fn from_runner_ignores_blank_agent_id() {
        let trusted = serde_json::json!({
            "_session_key": "session:parent",
            "_agent_id": "   ",
        });
        let context = ToolExecutionContext::from_runner(Some(&trusted), serde_json::json!({}));
        assert_eq!(context.sender_agent_id(), None);
    }
}
