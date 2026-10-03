use {
    super::*,
    secrecy::Secret,
    serde::{Deserialize, Serialize},
    std::collections::HashMap,
};

/// Validate an OpenAI Compatible provider section name.
///
/// The name is the user-specified slug: lowercase ASCII letters, digits, and
/// hyphens. `offered` is the providers table field, and `voice-*` is the
/// voice credential namespace in the shared key store.
pub fn openai_compatible_provider_name_error(name: &str) -> Option<&'static str> {
    let slug_ok = !name.is_empty()
        && !name.starts_with('-')
        && !name.ends_with('-')
        && name
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-');
    if !slug_ok {
        return Some("provider name must contain only lowercase letters, digits, and hyphens");
    }
    if name == "offered" {
        return Some("provider name 'offered' is reserved");
    }
    if name.starts_with("voice-") {
        return Some("provider name must not use the voice- prefix");
    }
    None
}

/// Lowercase a requested provider name and accept it when it is a valid slug.
pub fn parse_openai_compatible_provider_name(raw: &str) -> Result<String, &'static str> {
    let name = raw.trim().to_ascii_lowercase();
    match openai_compatible_provider_name_error(&name) {
        Some(error) => Err(error),
        None => Ok(name),
    }
}

/// LLM provider configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProvidersConfig {
    /// Optional allowlist of enabled providers. This also controls which
    /// providers are offered in web UI pickers (onboarding and "add provider"
    /// modal). Empty means all providers are enabled.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub offered: Vec<String>,

    /// Provider-specific settings keyed by the OpenAI Compatible provider name.
    #[serde(flatten)]
    pub providers: HashMap<String, ProviderEntry>,
}

/// How tool calling is handled for a provider.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ToolMode {
    /// Use the native tool calling API.
    #[default]
    Native,
    /// Use text-based tool calling (prompt injection + parse).
    Text,
    /// Disable all tool support for this provider.
    Off,
}

const fn is_default_tool_mode(v: &ToolMode) -> bool {
    matches!(v, ToolMode::Native)
}

/// Wire format for provider HTTP API.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WireApi {
    /// Standard OpenAI Chat Completions format (`/chat/completions`).
    #[default]
    ChatCompletions,
    /// OpenAI Responses API format (`/responses`).
    Responses,
}

/// Streaming transport for provider response streams.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderStreamTransport {
    /// Use HTTP + SSE streaming (current default).
    #[default]
    Sse,
    /// Use WebSocket mode when supported by the provider API.
    Websocket,
    /// Try WebSocket first, then fall back to SSE on transport/setup failure.
    Auto,
}

/// Configuration for a single LLM provider.
#[derive(Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProviderEntry {
    /// Whether this provider is enabled. Defaults to true.
    pub enabled: bool,

    /// Override the API key (optional; env var still takes precedence if set).
    #[serde(
        default,
        serialize_with = "serialize_option_secret",
        skip_serializing_if = "Option::is_none"
    )]
    pub api_key: Option<Secret<String>>,

    /// Override the base URL.
    pub base_url: Option<String>,

    /// Complete model records keyed by raw model ID.
    #[serde(default, skip_serializing_if = "ModelConfigMap::is_empty")]
    pub models: ModelConfigMap,

    /// Streaming transport for this provider (`sse`, `websocket`, `auto`).
    ///
    /// Defaults to `sse` for compatibility.
    #[serde(default, skip_serializing_if = "is_default_provider_stream_transport")]
    pub stream_transport: ProviderStreamTransport,

    /// Wire format for this provider (`chat-completions`, `responses`).
    ///
    /// - `chat-completions` (default): standard `/chat/completions` endpoint.
    /// - `responses`: OpenAI Responses API (`/responses`) format.
    #[serde(default, skip_serializing_if = "is_default_wire_api")]
    pub wire_api: WireApi,

    /// Optional alias for this provider instance.
    ///
    /// When set, this alias is used in metrics labels instead of the provider name.
    /// Useful when configuring multiple instances of the same provider type
    /// (e.g., "openai-work", "openai-personal").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,

    /// How tool calling is handled for this provider.
    ///
    /// - `native` (default): use native tool calling.
    /// - `text`: use text-based tool calling.
    /// - `off`: disable all tools for this provider.
    #[serde(default, skip_serializing_if = "is_default_tool_mode")]
    pub tool_mode: ToolMode,

    /// Tool policy override for this provider. When set, these allow/deny
    /// rules are merged on top of the global `[tools.policy]` for requests
    /// routed through this provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<ToolPolicyConfig>,
}

impl std::fmt::Debug for ProviderEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderEntry")
            .field("enabled", &self.enabled)
            .field("api_key", &self.api_key.as_ref().map(|_| "[REDACTED]"))
            .field("base_url", &self.base_url)
            .field("models", &self.models)
            .field("stream_transport", &self.stream_transport)
            .field("wire_api", &self.wire_api)
            .field("alias", &self.alias)
            .field("tool_mode", &self.tool_mode)
            .field("policy", &self.policy)
            .finish()
    }
}

impl Default for ProviderEntry {
    fn default() -> Self {
        Self {
            enabled: true,
            api_key: None,
            base_url: None,
            models: ModelConfigMap::new(),
            stream_transport: ProviderStreamTransport::Sse,
            wire_api: WireApi::ChatCompletions,
            alias: None,
            tool_mode: ToolMode::Native,
            policy: None,
        }
    }
}

impl ProvidersConfig {
    pub(crate) fn is_supported_name(name: &str) -> bool {
        openai_compatible_provider_name_error(name).is_none()
    }

    fn canonical_offered_name(value: &str) -> Option<String> {
        let normalized = value.trim().to_ascii_lowercase();
        Self::is_supported_name(&normalized).then_some(normalized)
    }

    pub(crate) fn invalid_provider_names(&self) -> Vec<(String, String)> {
        let invalid_sections = self
            .providers
            .keys()
            .filter(|name| !Self::is_supported_name(name))
            .map(|name| (format!("providers.{name}"), name.clone()));
        let invalid_offered = self
            .offered
            .iter()
            .enumerate()
            .filter(|(_, name)| Self::canonical_offered_name(name).is_none())
            .map(|(index, name)| (format!("providers.offered[{index}]"), name.clone()));
        invalid_sections.chain(invalid_offered).collect()
    }

    fn is_offered(&self, name: &str) -> bool {
        if self.offered.is_empty() {
            return true;
        }
        let Some(normalized) = Self::canonical_offered_name(name) else {
            return false;
        };
        self.offered
            .iter()
            .filter_map(|entry| Self::canonical_offered_name(entry))
            .any(|offered| offered == normalized)
    }

    fn provider_entry(&self, name: &str) -> Option<&ProviderEntry> {
        self.providers.get(name)
    }

    /// Check if a provider is enabled (defaults to true if not configured).
    pub fn is_enabled(&self, name: &str) -> bool {
        if !self.is_offered(name) {
            return false;
        }
        self.provider_entry(name).is_none_or(|e| e.enabled)
    }

    /// Get the configured entry for a provider, if any.
    pub fn get(&self, name: &str) -> Option<&ProviderEntry> {
        self.provider_entry(name)
    }
}
