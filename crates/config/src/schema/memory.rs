use {
    secrecy::Secret,
    serde::{Deserialize, Serialize},
};

/// Memory embedding provider configuration.
///
/// Controls which embedding provider the memory system uses.
/// If not configured, the system auto-detects from available providers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MemoryEmbeddingConfig {
    /// High-level memory orchestration style.
    pub style: MemoryStyle,
    /// Where agent-authored memory writes are allowed to land.
    pub agent_write_mode: AgentMemoryWriteMode,
    /// How Chelix writes the managed `USER.md` profile surface.
    pub user_profile_write_mode: UserProfileWriteMode,
    /// Embedding provider: "local", "openai", "custom", or None for auto-detect.
    #[serde(alias = "embedding_provider")]
    pub provider: Option<MemoryProvider>,
    /// Disable RAG embeddings and force keyword-only memory search.
    #[serde(default)]
    pub disable_rag: bool,
    /// Base URL for the embedding API (e.g. "https://api.openai.com/v1").
    #[serde(alias = "embedding_base_url")]
    pub base_url: Option<String>,
    /// Model name (e.g. "text-embedding-3-small" for OpenAI).
    #[serde(alias = "embedding_model")]
    pub model: Option<String>,
    /// API key (optional for local endpoints).
    #[serde(
        default,
        alias = "embedding_api_key",
        serialize_with = "crate::schema::serialize_option_secret",
        skip_serializing_if = "Option::is_none"
    )]
    pub api_key: Option<Secret<String>>,
    /// Hugging Face token for first-time local embedding model download.
    #[serde(
        default,
        alias = "HUGGINGFACE_API_KEY",
        serialize_with = "crate::schema::serialize_option_secret",
        skip_serializing_if = "Option::is_none"
    )]
    pub huggingface_api_key: Option<Secret<String>>,
    /// Citation mode for memory search results.
    pub citations: MemoryCitationsMode,
    /// Enable LLM reranking for hybrid search results.
    #[serde(default)]
    pub llm_reranking: bool,
    /// Merge strategy for hybrid search results.
    pub search_merge_strategy: MemorySearchMergeStrategy,
    /// Prefetch relevant memories at the start of each turn and inject them
    /// into the system prompt as `<recalled_context>`. Default: true.
    #[serde(default = "default_true")]
    pub enable_prefetch: bool,
    /// Maximum number of memories to prefetch per turn. Default: 3.
    #[serde(default = "default_prefetch_limit")]
    pub prefetch_limit: usize,
}

impl Default for MemoryEmbeddingConfig {
    fn default() -> Self {
        Self {
            style: MemoryStyle::default(),
            agent_write_mode: AgentMemoryWriteMode::default(),
            user_profile_write_mode: UserProfileWriteMode::default(),
            provider: None,
            disable_rag: false,
            base_url: None,
            model: None,
            api_key: None,
            huggingface_api_key: None,
            citations: MemoryCitationsMode::default(),
            llm_reranking: false,
            search_merge_strategy: MemorySearchMergeStrategy::default(),
            enable_prefetch: true,
            prefetch_limit: 3,
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_prefetch_limit() -> usize {
    3
}

/// High-level orchestration style for prompt memory and memory tools.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MemoryStyle {
    /// Current behavior: inject `MEMORY.md` into the prompt and expose memory tools.
    #[default]
    Hybrid,
    /// Inject `MEMORY.md` into the prompt, but hide memory tools.
    PromptOnly,
    /// Skip prompt injection and rely on memory tools for recall.
    SearchOnly,
    /// Disable both prompt memory injection and memory tools.
    Off,
}

/// Where agent-authored long-term memory writes can be stored.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentMemoryWriteMode {
    /// Allow both prompt-visible `MEMORY.md` writes and searchable `memory/*.md` notes.
    #[default]
    Hybrid,
    /// Restrict writes to prompt-visible `MEMORY.md`.
    PromptOnly,
    /// Restrict writes to searchable `memory/*.md` notes.
    SearchOnly,
    /// Disable agent-authored memory writes entirely.
    Off,
}

/// How Chelix writes the managed `USER.md` profile surface.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UserProfileWriteMode {
    /// Allow both explicit settings saves and silent browser/channel enrichment.
    #[default]
    ExplicitAndAuto,
    /// Allow explicit settings saves, but disable silent browser/channel enrichment.
    ExplicitOnly,
    /// Do not write `USER.md`; keep user profile only in `chelix.toml`.
    Off,
}

impl UserProfileWriteMode {
    #[must_use]
    pub fn allows_explicit_write(self) -> bool {
        !matches!(self, Self::Off)
    }

    #[must_use]
    pub fn allows_auto_write(self) -> bool {
        matches!(self, Self::ExplicitAndAuto)
    }
}

/// Citation mode for memory search results.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MemoryCitationsMode {
    /// Always include citations in memory search results.
    On,
    /// Never include citations in memory search results.
    Off,
    /// Include citations when results come from multiple files.
    #[default]
    Auto,
}

/// Embedding provider for memory/RAG features.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MemoryProvider {
    /// Built-in local embeddings via the managed sidecar.
    Local,
    /// OpenAI embedding API.
    #[serde(rename = "openai")]
    OpenAi,
    /// Generic OpenAI-compatible endpoint.
    Custom,
}

/// Strategy for merging keyword and vector search results.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MemorySearchMergeStrategy {
    /// Reciprocal rank fusion.
    #[default]
    Rrf,
    /// Linear blend of raw keyword and vector scores.
    Linear,
}
