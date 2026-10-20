use {
    super::*,
    secrecy::Secret,
    serde::{Deserialize, Serialize},
};

/// Gateway server configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    /// Address to bind to. Defaults to "127.0.0.1".
    pub bind: String,
    /// Port to listen on. When a new config is created, a random available port
    /// is generated so each installation gets a unique port.
    pub port: u16,
    /// Enable verbose Axum/Tower HTTP request logs (`http_request` spans).
    /// Useful for debugging redirects and request flow.
    pub http_request_logs: bool,
    /// Enable WebSocket request/response logs (`ws:` entries).
    /// Useful for debugging RPC calls from the web UI.
    pub ws_request_logs: bool,
    /// Maximum number of log entries kept in the in-memory ring buffer.
    /// Older entries are persisted to disk and available via the web UI.
    /// Defaults to 1000. Increase for busy servers, decrease for memory-constrained devices.
    #[serde(default = "default_log_buffer_size")]
    pub log_buffer_size: usize,
    /// URL of the releases manifest (`releases.json`) used by the update checker.
    ///
    /// Defaults to `https://github.com/agentics-skills/chelix` when unset.
    pub update_releases_url: Option<String>,
    /// Maximum number of SQLite pool connections. Lower values reduce memory
    /// usage for personal gateways. Defaults to 5.
    #[serde(default = "default_db_pool_max_connections")]
    pub db_pool_max_connections: u32,
    /// Base URL for the Shiki syntax-highlighting library loaded by the web UI.
    ///
    /// Defaults to `https://esm.sh/shiki@3.2.1?bundle` when unset.
    /// Set to an alternative CDN or a self-hosted URL to override.
    pub shiki_cdn_url: Option<String>,
    /// Enable or disable the host terminal in the web UI.
    ///
    /// Defaults to `true`. Set to `false` to prevent the web UI from
    /// offering an unsandboxed shell.
    #[serde(default = "default_terminal_enabled")]
    pub terminal_enabled: bool,
    /// Maximum number of terminal lines retained by the web UI.
    ///
    /// Defaults to 300,000 lines.
    #[serde(default = "default_scrollback_lines")]
    pub scrollback_lines: u32,
    /// Public URL when running behind a reverse proxy (e.g.
    /// `https://chelix.example.com`). Used to derive the WebAuthn RP ID
    /// and origin so passkey auth works with the proxy's public hostname.
    pub external_url: Option<String>,
}

fn default_log_buffer_size() -> usize {
    1000
}

fn default_db_pool_max_connections() -> u32 {
    5
}

fn default_terminal_enabled() -> bool {
    true
}

const fn default_scrollback_lines() -> u32 {
    300_000
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1".into(),
            port: 0, // Will be replaced with a random port when config is created
            http_request_logs: false,
            ws_request_logs: false,
            log_buffer_size: default_log_buffer_size(),
            update_releases_url: None,
            db_pool_max_connections: default_db_pool_max_connections(),
            shiki_cdn_url: None,
            terminal_enabled: default_terminal_enabled(),
            scrollback_lines: default_scrollback_lines(),
            external_url: None,
        }
    }
}

impl ServerConfig {
    /// Returns whether the web UI terminal is enabled.
    pub fn is_terminal_enabled(&self) -> bool {
        self.terminal_enabled
    }

    /// Returns the configured external URL. Trailing slashes are stripped
    /// so the result can be used directly as a WebAuthn origin.
    pub fn effective_external_url(&self) -> Option<String> {
        self.external_url
            .as_deref()
            .map(|raw| raw.trim_end_matches('/').to_string())
    }
}

/// Cron scheduler configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CronConfig {
    /// Maximum number of jobs that can be created within the rate limit window.
    /// Defaults to 10.
    pub rate_limit_max: usize,
    /// Rate limit window in seconds. Defaults to 60 (1 minute).
    pub rate_limit_window_secs: u64,
    /// Number of days to retain cron session data before auto-cleanup.
    /// Set to `None` (or 0) to disable retention pruning. Defaults to 7 days.
    pub session_retention_days: Option<u64>,
    /// Whether to auto-prune sandbox containers after cron job completion.
    /// Per-job `auto_prune_container` overrides this global default.
    /// Defaults to true.
    #[serde(default = "default_true")]
    pub auto_prune_cron_containers: bool,
}

impl Default for CronConfig {
    fn default() -> Self {
        Self {
            rate_limit_max: 10,
            rate_limit_window_secs: 60,
            session_retention_days: Some(7),
            auto_prune_cron_containers: true,
        }
    }
}

/// Home Assistant integration configuration.
///
/// ```toml
/// [home_assistant]
/// enabled = true
/// default_instance = "home"
///
/// [home_assistant.instances.home]
/// url = "http://homeassistant.local:8123"
/// token = "eyJ..."
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct HomeAssistantConfig {
    /// Whether Home Assistant integration is enabled.
    pub enabled: bool,
    /// Default instance name when none is specified.
    pub default_instance: Option<String>,
    /// Named HA instances.
    #[serde(default)]
    pub instances: HashMap<String, HomeAssistantAccountConfig>,
}

impl HomeAssistantConfig {
    /// Resolve which account to use given an optional explicit name.
    ///
    /// Resolution order: explicit `instance` → `default_instance` →
    /// sole instance if only one exists.
    #[must_use]
    pub fn resolve_account<'a>(
        &'a self,
        instance: Option<&'a str>,
    ) -> Option<(&'a str, &'a HomeAssistantAccountConfig)> {
        let name = instance.or(self.default_instance.as_deref()).or_else(|| {
            if self.instances.len() == 1 {
                self.instances.keys().next().map(String::as_str)
            } else {
                None
            }
        })?;

        self.instances.get(name).map(|a| (name, a))
    }
}

/// Configuration for a single Home Assistant instance.
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HomeAssistantAccountConfig {
    /// HA instance URL (e.g. "http://homeassistant.local:8123").
    pub url: Option<String>,
    /// Long-lived access token.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_option_secret",
        deserialize_with = "deserialize_option_secret"
    )]
    pub token: Option<Secret<String>>,
    /// HTTP request timeout in seconds.
    #[serde(default = "default_ha_timeout")]
    pub timeout_seconds: u64,
}

impl Default for HomeAssistantAccountConfig {
    fn default() -> Self {
        Self {
            url: None,
            token: None,
            timeout_seconds: default_ha_timeout(),
        }
    }
}

impl std::fmt::Debug for HomeAssistantAccountConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HomeAssistantAccountConfig")
            .field("url", &self.url)
            .field("token", &self.token.as_ref().map(|_| "[REDACTED]"))
            .field("timeout_seconds", &self.timeout_seconds)
            .finish()
    }
}

fn default_ha_timeout() -> u64 {
    10
}
