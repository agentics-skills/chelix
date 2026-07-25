use super::{AgentToolControls, ToolChoice};

/// Per-request controls for a non-streaming completion.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompletionOptions {
    pub tool_controls: AgentToolControls,
    pub max_output_tokens: Option<u32>,
}

impl CompletionOptions {
    #[must_use]
    pub fn with_max_output_tokens(max_output_tokens: u32) -> Self {
        Self {
            max_output_tokens: Some(max_output_tokens),
            ..Self::default()
        }
    }

    /// Preserve the default provider behavior for transports that do not
    /// implement forced tool selection.
    pub fn reject_forced_tool_choice(&self, provider_name: &str) -> anyhow::Result<()> {
        if matches!(
            self.tool_controls.tool_choice,
            Some(ToolChoice::Tool { .. } | ToolChoice::Any)
        ) {
            anyhow::bail!("provider {provider_name} does not support forced tool_choice");
        }
        Ok(())
    }
}

impl From<AgentToolControls> for CompletionOptions {
    fn from(tool_controls: AgentToolControls) -> Self {
        Self {
            tool_controls,
            max_output_tokens: None,
        }
    }
}
