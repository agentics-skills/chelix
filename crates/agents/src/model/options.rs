use super::ToolChoice;

/// Per-request controls for streaming completion.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompletionOptions {
    pub tool_choice: Option<ToolChoice>,
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

    /// Validate provider support for forced tool selection.
    pub fn reject_forced_tool_choice(&self, provider_name: &str) -> anyhow::Result<()> {
        if matches!(
            self.tool_choice,
            Some(ToolChoice::Tool { .. } | ToolChoice::Any)
        ) {
            anyhow::bail!("provider {provider_name} does not support forced tool_choice");
        }
        Ok(())
    }
}
