use {
    secrecy::{ExposeSecret, Secret},
    serde::{Deserialize, Serialize},
};

/// Memory embedding provider configuration.
///
/// Embeddings are used only when `url`, `api_key`, and `dimensions` are all set.
/// If all three are unset, search is keyword-only.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MemoryEmbeddingConfig {
    /// High-level memory orchestration style.
    pub style: MemoryStyle,
    /// Where agent-authored memory writes are allowed to land.
    pub agent_write_mode: AgentMemoryWriteMode,
    /// How Chelix writes the managed `USER.md` profile surface.
    pub user_profile_write_mode: UserProfileWriteMode,
    /// Disable RAG embeddings and force keyword-only memory search.
    #[serde(default)]
    pub disable_rag: bool,
    /// Base URL of the remote embedding provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Bearer token sent to the remote embedding provider.
    #[serde(
        default,
        serialize_with = "crate::schema::serialize_option_secret",
        skip_serializing_if = "Option::is_none"
    )]
    pub api_key: Option<Secret<String>>,
    /// Embedding vector width expected from the provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dimensions: Option<usize>,
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
            disable_rag: false,
            url: None,
            api_key: None,
            dimensions: None,
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

/// Remote embedding endpoint selected by [`MemoryEmbeddingConfig::embedding_endpoint`].
#[derive(Debug, Clone)]
pub struct EmbeddingEndpoint {
    pub url: String,
    pub api_key: Secret<String>,
    pub dimensions: usize,
}

impl MemoryEmbeddingConfig {
    /// All three embedding fields are unset, or all three are present.
    /// Does not parse `url`, so an unresolved `${VAR}` placeholder is accepted.
    pub fn embedding_fields_complete(&self) -> Result<(), String> {
        match (&self.url, &self.api_key, self.dimensions) {
            (None, None, None) => Ok(()),
            (Some(url), Some(api_key), Some(dimensions)) => {
                if url.is_empty() {
                    return Err("memory.url is empty".into());
                }
                if url != url.trim() {
                    return Err("memory.url must not have leading or trailing whitespace".into());
                }
                if api_key.expose_secret().is_empty() {
                    return Err("memory.api_key is empty".into());
                }
                if dimensions < 1 {
                    return Err("memory.dimensions must be >= 1".into());
                }
                Ok(())
            },
            _ => Err("memory embedding requires url, api_key, and dimensions together".into()),
        }
    }

    /// `Ok(None)` when url, api_key, and dimensions are all unset.
    /// `Ok(Some)` when all three are present and `url` is an http(s) URL.
    pub fn embedding_endpoint(&self) -> Result<Option<EmbeddingEndpoint>, String> {
        self.embedding_fields_complete()?;
        match (&self.url, &self.api_key, self.dimensions) {
            (None, None, None) => Ok(None),
            (Some(url), Some(api_key), Some(dimensions)) => {
                let parsed = url::Url::parse(url)
                    .map_err(|error| format!("memory.url is invalid: {error}"))?;
                if parsed.scheme() != "http" && parsed.scheme() != "https" {
                    return Err("memory.url scheme must be http or https".into());
                }
                if parsed.host_str().is_none_or(|host| host.is_empty()) {
                    return Err("memory.url host is empty".into());
                }
                Ok(Some(EmbeddingEndpoint {
                    url: url.clone(),
                    api_key: api_key.clone(),
                    dimensions,
                }))
            },
            _ => Err("memory embedding requires url, api_key, and dimensions together".into()),
        }
    }
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
