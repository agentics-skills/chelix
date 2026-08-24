use {
    super::*,
    serde::{Deserialize, Serialize},
    std::collections::HashMap,
};

pub const DEFAULT_MAX_TOOLS_THRESHOLD: usize = 128;

const RESERVED_AGENT_IDS: &[&str] = &["default"];
const INVALID_AGENT_ID_MESSAGE: &str = "agent id must use lowercase letters, numbers, and hyphens, and cannot start or end with a hyphen";

/// Validate an agent ID for use as a dynamic key under `[agents]`.
///
/// Static `AgentsConfig` field names are reserved because TOML cannot contain
/// both `[agents].<field>` and `[agents.<field>]`.
pub fn validate_agent_id(id: &str) -> Result<(), &'static str> {
    if RESERVED_AGENT_IDS.contains(&id) {
        return Err("agent id is reserved by the [agents] configuration table");
    }

    let valid = !id.is_empty()
        && id.len() <= 80
        && !id.starts_with('-')
        && !id.ends_with('-')
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    if valid {
        Ok(())
    } else {
        Err(INVALID_AGENT_ID_MESSAGE)
    }
}

/// User-owned agent registry.
///
/// `default` selects the agent used for new sessions. Every other key under
/// `[agents]` is an agent ID.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentsConfig {
    pub default: String,
    #[serde(flatten)]
    pub entries: HashMap<String, AgentConfig>,
}

/// Per-request tool choice requested by the agent harness.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolChoice {
    Auto,
    Any,
    None,
    Tool { name: String },
}

pub fn tool_choice_from_request_params(
    request_params: &serde_json::Value,
) -> serde_json::Result<Option<ToolChoice>> {
    request_params
        .get("tool_choice")
        .cloned()
        .map(serde_json::from_value::<ToolChoice>)
        .transpose()
}

impl AgentsConfig {
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&AgentConfig> {
        self.entries.get(id)
    }

    #[must_use]
    pub fn default_agent(&self) -> Option<&AgentConfig> {
        self.get(&self.default)
    }
}

/// Identifies an MCP server by its configuration key.
///
/// Wraps the server name used as the key in `[mcp.servers.<name>]` and
/// in tool names like `mcp__<name>__<tool>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct McpServerId(String);

impl McpServerId {
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Tool-policy deny pattern that blocks all tools from this server.
    #[must_use]
    pub fn to_deny_pattern(&self) -> String {
        format!("mcp__{}__*", self.0)
    }
}

impl std::fmt::Display for McpServerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for McpServerId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl From<&str> for McpServerId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl From<String> for McpServerId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl std::borrow::Borrow<str> for McpServerId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

/// Per-agent MCP server access control.
///
/// Controls which MCP servers are visible to this agent. Translates to
/// tool policy deny patterns (`mcp__<server>__*`) at resolution time,
/// so the agent never sees excluded servers' tools in its context.
///
/// ```toml
/// # Allow-list: only these servers are visible
/// [agents.my-agent.mcp]
/// allow_servers = ["github", "memory"]
///
/// # Deny-list: all servers except these
/// [agents.my-agent.mcp]
/// deny_servers = ["home-assistant"]
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum AgentMcpPolicy {
    /// No restrictions — all MCP servers are visible (default).
    #[default]
    All,
    /// Only the listed servers are visible. All others are denied.
    Allow(Vec<McpServerId>),
    /// All servers except the listed ones are visible.
    Deny(Vec<McpServerId>),
}

impl AgentMcpPolicy {
    /// Returns `true` when no MCP restrictions are configured.
    #[must_use]
    pub fn is_all(&self) -> bool {
        matches!(self, Self::All)
    }
}

impl Serialize for AgentMcpPolicy {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        match self {
            Self::All => {
                let map = serializer.serialize_map(Some(0))?;
                map.end()
            },
            Self::Allow(servers) => {
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("allow_servers", servers)?;
                map.end()
            },
            Self::Deny(servers) => {
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("deny_servers", servers)?;
                map.end()
            },
        }
    }
}

impl<'de> Deserialize<'de> for AgentMcpPolicy {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        // Use Option to distinguish "field absent" from "field present but empty".
        // `allow_servers = []` means "allow no MCP servers" (deny all),
        // while omitting the field entirely means "no restriction" (All).
        #[derive(Deserialize)]
        struct Raw {
            allow_servers: Option<Vec<McpServerId>>,
            deny_servers: Option<Vec<McpServerId>>,
        }
        let raw = Raw::deserialize(deserializer)?;
        match (raw.allow_servers, raw.deny_servers) {
            (None, None) => Ok(Self::All),
            (Some(servers), None) => Ok(Self::Allow(servers)),
            (None, Some(servers)) => Ok(Self::Deny(servers)),
            (Some(_), Some(_)) => Err(serde::de::Error::custom(
                "mcp: allow_servers and deny_servers are mutually exclusive",
            )),
        }
    }
}

/// Tool policy and lazy schema visibility for an agent.
///
/// Applied as Layer 3 in the 6-layer policy resolution for all sessions
/// belonging to this agent. When both `allow` and `deny` are specified,
/// `allow` acts as a whitelist and `deny` further removes from that list.
/// Glob patterns are supported (e.g. `"mcp__*"` to deny all MCP tools).
/// In lazy registry mode, `preload` names parameter schemas to expose from the
/// already-filtered registry at the start of a run.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentToolPolicy {
    /// Tools to allow (whitelist). If empty, all tools are allowed.
    #[serde(default)]
    pub allow: Vec<String>,
    /// Tools to deny (blacklist). Applied after `allow`.
    #[serde(default)]
    pub deny: Vec<String>,
    /// Tool schemas exposed immediately in lazy registry mode.
    ///
    /// Names are resolved against the effective registry after all allow/deny
    /// policy layers, so this list cannot make a filtered tool visible.
    #[serde(default)]
    pub preload: Vec<String>,
}

/// Session access policy configuration for an agent.
///
/// Controls which sessions an agent can see and interact with via
/// the `sessions_list`, `sessions_history`, and `sessions_send` tools.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionAccessPolicyConfig {
    /// Only see sessions with keys matching this prefix.
    pub key_prefix: Option<String>,
    /// Explicit session keys this agent can access (in addition to prefix).
    #[serde(default)]
    pub allowed_keys: Vec<String>,
    /// Whether the agent can send messages to sessions.
    #[serde(default = "default_true")]
    pub can_send: bool,
    /// Whether the agent can access sessions from other agents.
    #[serde(default)]
    pub cross_agent: bool,
}

impl Default for SessionAccessPolicyConfig {
    fn default() -> Self {
        Self {
            key_prefix: None,
            allowed_keys: Vec::new(),
            can_send: true,
            cross_agent: false,
        }
    }
}

/// Per-agent skill access control.
///
/// ```toml
/// # Only allow specific skills
/// [agents.kids.skills]
/// allow = ["research"]
///
/// # Deny specific skills
/// [agents.admin.skills]
/// deny = ["gaming", "social-media"]
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentSkillPolicy {
    /// When `Some`, only these skills (by name or category) are available.
    /// `Some(vec![])` means "no skills allowed" (deny all).
    /// `None` (absent from config) means "no restriction".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow: Option<Vec<String>>,
    /// Skills (by name or category) to deny from this agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deny: Option<Vec<String>>,
}

impl AgentSkillPolicy {
    /// Returns `true` when no skill filtering is configured.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.allow.is_none() && self.deny.is_none()
    }
}

/// Complete configuration for one user-owned agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConfig {
    pub name: String,
    #[serde(default)]
    pub emoji: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub voice_persona_id: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub tools: AgentToolPolicy,
    /// Maximum LLM-initiated tool calls per agent loop segment.
    pub max_tools_threshold: usize,
    /// Timeout in seconds for sessions using this agent.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    /// Maximum in-context bytes per tool result before truncation.
    /// Falls back to `tools.max_tool_result_bytes`.
    #[serde(default)]
    pub max_tool_result_bytes: Option<usize>,
    /// Session access policy for inter-agent communication.
    #[serde(default)]
    pub sessions: Option<SessionAccessPolicyConfig>,
    /// Reasoning/thinking effort level for models that support extended thinking.
    ///
    /// Controls extended thinking for models that support it (e.g. Claude Opus,
    /// OpenAI o-series). Higher values enable deeper reasoning but increase
    /// latency and token usage.
    #[serde(default)]
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Per-agent MCP server access control.
    ///
    /// Controls which MCP servers are visible to this agent:
    /// - `All` (default) — no restrictions, all MCP servers visible.
    /// - `Allow(servers)` — only listed servers visible; others denied.
    /// - `Deny(servers)` — all servers visible except listed ones.
    #[serde(default, skip_serializing_if = "AgentMcpPolicy::is_all")]
    pub mcp: AgentMcpPolicy,
    /// Per-agent skill access control.
    ///
    /// Controls which skills are visible to this agent. When `allow` is
    /// non-empty, only listed skills are available. `deny` removes skills
    /// by name or category.
    #[serde(default, skip_serializing_if = "AgentSkillPolicy::is_empty")]
    pub skills: AgentSkillPolicy,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            emoji: None,
            description: None,
            voice_persona_id: None,
            model: None,
            tools: AgentToolPolicy::default(),
            max_tools_threshold: DEFAULT_MAX_TOOLS_THRESHOLD,
            timeout_secs: None,
            max_tool_result_bytes: None,
            sessions: None,
            reasoning_effort: None,
            mcp: AgentMcpPolicy::default(),
            skills: AgentSkillPolicy::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_ids_reject_reserved_and_invalid_keys() {
        assert!(validate_agent_id("qa-2").is_ok());
        assert_eq!(
            validate_agent_id("default"),
            Err("agent id is reserved by the [agents] configuration table")
        );
        assert_eq!(validate_agent_id("QA"), Err(INVALID_AGENT_ID_MESSAGE));
        assert_eq!(validate_agent_id("-qa"), Err(INVALID_AGENT_ID_MESSAGE));
    }

    #[test]
    fn tool_choice_parses_from_request_params() {
        let params = serde_json::json!({
            "tool_choice": { "type": "tool", "name": "overwrite_file" }
        });

        assert!(matches!(
            tool_choice_from_request_params(&params),
            Ok(Some(ToolChoice::Tool { name })) if name == "overwrite_file"
        ));
    }

    #[test]
    fn tool_choice_parses_any_variant() {
        let params = serde_json::json!({
            "tool_choice": { "type": "any" }
        });

        assert!(matches!(
            tool_choice_from_request_params(&params),
            Ok(Some(ToolChoice::Any))
        ));
    }

    #[test]
    fn absent_tool_choice_returns_none() {
        assert!(matches!(
            tool_choice_from_request_params(&serde_json::json!({})),
            Ok(None)
        ));
    }

    #[test]
    fn invalid_tool_choice_returns_error() {
        let params = serde_json::json!({
            "tool_choice": { "type": "tool" }
        });

        assert!(tool_choice_from_request_params(&params).is_err());
    }
}
