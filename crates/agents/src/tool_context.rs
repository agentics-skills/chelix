use chelix_sessions::SessionKey;

/// Trusted execution context supplied by the agent runner separately from a
/// tool's public JSON arguments.
#[derive(Debug, Clone)]
pub struct ToolExecutionContext {
    session_key: Option<SessionKey>,
    execution_arguments: Option<serde_json::Value>,
}

impl ToolExecutionContext {
    /// Build execution context for a direct, session-scoped tool invocation.
    #[must_use]
    pub fn for_session(session_key: SessionKey) -> Self {
        Self {
            session_key: Some(session_key),
            execution_arguments: None,
        }
    }

    /// Return the canonical session key when this run has session context.
    #[must_use]
    pub const fn session_key(&self) -> Option<&SessionKey> {
        self.session_key.as_ref()
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
        Self {
            session_key,
            execution_arguments: Some(execution_arguments),
        }
    }

    pub(crate) const fn execution_arguments(&self) -> Option<&serde_json::Value> {
        self.execution_arguments.as_ref()
    }
}
