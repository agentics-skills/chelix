use serde::{Deserialize, Serialize};

/// Exact values used by the agent loop's automatic checkpoint check.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextBudgetMetadata {
    pub context_window: u32,
    pub max_input_tokens: u32,
    pub max_output_tokens: u32,
    pub compaction_ratio: usize,
    pub prompt_tokens: usize,
    pub tool_schema_tokens: usize,
    pub available_input_tokens: usize,
    pub compaction_budget: usize,
    pub usage_percent: usize,
    pub compaction_required: bool,
}
