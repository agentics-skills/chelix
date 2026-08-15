use serde::{Deserialize, Serialize};

/// Chat configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ChatConfig {
    /// Automatically generate a session title after the first exchange.
    #[serde(default = "default_auto_title")]
    pub auto_title: bool,
    /// How `MEMORY.md` is loaded into the prompt for an ongoing session.
    #[serde(default = "default_prompt_memory_mode")]
    pub prompt_memory_mode: PromptMemoryMode,
    /// Maximum characters from each workspace prompt file (`AGENTS.md`, `TOOLS.md`).
    #[serde(default = "default_workspace_file_max_chars")]
    pub workspace_file_max_chars: usize,
    /// Preferred model IDs to show first in selectors (full or raw model IDs).
    pub priority_models: Vec<String>,
}

fn default_auto_title() -> bool {
    true
}

fn default_prompt_memory_mode() -> PromptMemoryMode {
    PromptMemoryMode::LiveReload
}

fn default_workspace_file_max_chars() -> usize {
    32_000
}

impl Default for ChatConfig {
    fn default() -> Self {
        Self {
            auto_title: default_auto_title(),
            prompt_memory_mode: default_prompt_memory_mode(),
            workspace_file_max_chars: default_workspace_file_max_chars(),
            priority_models: Vec::new(),
        }
    }
}

/// How prompt memory is loaded across turns in the same session.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PromptMemoryMode {
    /// Reload `MEMORY.md` from disk before each turn.
    #[default]
    LiveReload,
    /// Freeze the initial `MEMORY.md` content for the lifetime of the session.
    FrozenAtSessionStart,
}

/// How tool schemas are presented to the model.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ToolRegistryMode {
    /// All tool schemas are sent to the model on every turn (default).
    #[default]
    Full,
    /// The full tool catalog (names + descriptions) is always advertised, but
    /// parameter schemas are deferred: only `get_tool` and schemas the model has
    /// fetched by exact name are sent. Saves input tokens with many tools.
    Lazy,
}

/// Auxiliary model assignments for side tasks.
///
/// Route compression, title generation, and vision to cheaper/faster models
/// while keeping the main session on a more capable model. Falls back to the
/// session's primary provider when a field is `None`.
///
/// ```toml
/// [auxiliary]
/// title_generation = "openrouter/openai/gpt-5-mini"
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AuxiliaryModelsConfig {
    /// Model for session title generation.
    pub title_generation: Option<String>,
    /// Model for vision/image analysis tasks.
    pub vision: Option<String>,
}
