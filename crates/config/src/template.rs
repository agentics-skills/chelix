//! Default configuration template with all options documented.
//!
//! This template is used when creating a new config file. It contains only
//! user overrides — built-in defaults live in `defaults.toml` (Chelix-managed)
//! and should not be duplicated here.
//!
//! Uncommenting a built-in default here creates a local override that shadows
//! future built-in updates on upgrade.

/// Generate the default config template with a specific port.
///
/// The template is override-only: only the installation-specific port is set
/// as an active value. All other settings are commented out with documentation
/// so users can see what's available without accidentally freezing defaults.
pub fn default_config_template(port: u16) -> String {
    format!(
        r##"# Chelix User Configuration
# =========================
# This file contains YOUR overrides only. Built-in defaults are in
# defaults.toml (Chelix-managed, regenerated on every startup).
#
# Uncomment and modify settings to override the built-in defaults.
# Changes require a restart to take effect.
#
# ⚠️  Uncommenting a built-in default here creates a local override that
#     shadows future built-in improvements on upgrade. Only uncomment
#     settings you intentionally want to control.
#
# Environment variable substitution is supported: ${{ENV_VAR}}
# Example: api_key = "${{ANTHROPIC_API_KEY}}"

# ══════════════════════════════════════════════════════════════════════════════
# SERVER
# ══════════════════════════════════════════════════════════════════════════════

[server]
port = {port}                           # Port number (auto-generated for this installation)
# bind = "127.0.0.1"                # Address to bind to ("0.0.0.0" for all interfaces)
# http_request_logs = false              # Enable verbose Axum HTTP request/response logs (debugging)
# ws_request_logs = false                # Enable WebSocket RPC request/response logs (debugging)
# terminal_enabled = true                # Enable interactive host terminal in Settings > Terminal
                                         # Set to false to disable the unsandboxed shell in the web UI.
                                         # NOTE: this can be re-enabled via the web UI config editor.
                                         # For hard lockdown, set CHELIX_TERMINAL_DISABLED=1 (env var
                                         # takes precedence and cannot be changed from the web UI).
# update_releases_url = "https://github.com/agentics-skills/chelix"  # Override releases manifest URL
# external_url = "https://chelix.example.com"  # Public URL when behind a reverse proxy.
                                                 # Used for WebAuthn passkey origins.
                                                 # Env var CHELIX_EXTERNAL_URL takes precedence.

# ══════════════════════════════════════════════════════════════════════════════
# UPSTREAM PROXY
# ══════════════════════════════════════════════════════════════════════════════
# Route all outbound traffic (providers, channels, tools, OAuth) through a
# proxy. Supports http://, https://, socks5://, socks5h:// schemes.
# Authentication via URL: "http://user:pass@host:port"
# When unset, reqwest honours HTTP_PROXY / HTTPS_PROXY / ALL_PROXY env vars.

# upstream_proxy = "http://127.0.0.1:1080"

# ══════════════════════════════════════════════════════════════════════════════
# AUTHENTICATION
# ══════════════════════════════════════════════════════════════════════════════

# [auth]
# disabled = false                  # true = disable auth entirely (DANGEROUS if exposed)
                                    # When disabled, anyone with network access can use chelix
# vault_enabled = true              # true = encrypt stored secrets at rest using the password vault
#                                   # Set false to keep password auth without requiring vault unlocks after restart.

# ══════════════════════════════════════════════════════════════════════════════
# GRAPHQL
# ══════════════════════════════════════════════════════════════════════════════

# [graphql]
# enabled = false                   # Enable GraphQL endpoint (/graphql for HTTP + WebSocket)
                                    # Can be toggled at runtime in Settings > GraphQL

# ══════════════════════════════════════════════════════════════════════════════
# TLS / HTTPS
# ══════════════════════════════════════════════════════════════════════════════

# [tls]
# enabled = true                    # Enable HTTPS (recommended)
# auto_generate = true              # Auto-generate local CA and server certificate
# public_ip = "203.0.113.10"        # Optional IP SAN for direct https://IP access
# http_redirect_port = 18790        # Optional override (default: server.port + 1)
# cert_path = "/path/to/cert.pem"   # Custom certificate file (overrides auto-gen)
# key_path = "/path/to/key.pem"     # Custom private key file
# ca_cert_path = "/path/to/ca.pem"  # CA certificate for trust instructions

# ══════════════════════════════════════════════════════════════════════════════
# AGENT IDENTITY
# ══════════════════════════════════════════════════════════════════════════════
# Customize your agent's personality. These are typically set during onboarding.

# [identity]
# name = "chelix"                   # Agent's display name
# emoji = "🦊"                      # Agent's emoji/avatar
# theme = "wise owl"                # Theme for agent personality (e.g. wise owl, chill fox)
# soul = ""                         # Freeform personality text injected into system prompt
                                    # Use this for custom instructions, tone, or behavior

# ══════════════════════════════════════════════════════════════════════════════
# USER PROFILE
# ══════════════════════════════════════════════════════════════════════════════
# Information about you. Set during onboarding.

# [user]
# name = "Your Name"                # Your name (used in conversations)
# timezone = "America/New_York"     # Your timezone (IANA format)

# ══════════════════════════════════════════════════════════════════════════════
# LLM PROVIDERS
# ══════════════════════════════════════════════════════════════════════════════
# Configure API keys and settings for each LLM provider.
# API keys can also be set via environment variables (preferred for security).
#
# Each provider supports:
#   enabled   - Whether to use this provider (default: true)
#   api_key   - API key (or use env var like ANTHROPIC_API_KEY)
#   base_url  - Override API endpoint
#   models.<model_id> - Ordered allowlist entry with per-model metadata (optional)
#   fetch_models - Discover models from provider API when available (default: true)
#   stream_transport - Streaming transport: "sse", "websocket", or "auto" (default: "sse")
#   alias     - Custom name for metrics labels (useful for multiple instances)
#   strict_tools - Force strict/non-strict tool schemas (default: auto-detect per provider)
#   policy    - Per-provider tool policy override (allow/deny lists)
#   probe_timeout_secs - Timeout for completion-based model probes (default: 30s).
#
# Declare selected models only as [providers.<name>.models."<raw-model-id>"]
# tables. Tables are evaluated in declaration order. Configuration metadata wins
# field by field; /models discovery fills missing fields; optional defaults apply
# last. Models are excluded unless context_length, max_input_tokens,
# max_output_tokens, and reasoning.supported_efforts resolve. With no model
# tables, every discovered model with complete metadata is accepted.

# [providers]
# offered = ["github-copilot", "openai-codex", "openai", "anthropic", "openrouter", "moonshot", "zai"]
                                    # Enabled providers and those shown in onboarding/picker UI ([] = enable/show all)
# show_legacy_models = true         # Show models older than 1 year in the chat model selector (they always appear in Settings)
# All available providers (canonical list in schema/providers.rs):
#   "anthropic", "openai", "gemini", "xai", "deepinfra",
#   "openrouter", "moonshot", "zai", "zai-code", "alibaba-coding",
#   "openai-codex", "github-copilot", "kimi-code"

# ── Anthropic (Claude) ────────────────────────────────────────
# [providers.anthropic]
# enabled = true
# api_key = "sk-ant-..."                      # Or set ANTHROPIC_API_KEY env var
# fetch_models = true                          # Set false to skip remote discovery
# base_url = "https://api.anthropic.com"     # API endpoint
# alias = "anthropic"                         # Custom name for metrics
# cache_retention = "short"                    # Prompt caching: "none" | "short" | "long"
# policy.deny = ["execute_command"]            # Deny specific tools when using this provider
# policy.allow = []                            # Restrict to only these tools (empty = all allowed)
# [providers.anthropic.models."claude-sonnet-4-5-20250929"]

# ── OpenAI ────────────────────────────────────────────────────
# [providers.openai]
# enabled = true
# api_key = "sk-..."                          # Or set OPENAI_API_KEY env var
# fetch_models = true
# stream_transport = "sse"                     # "sse" | "websocket" | "auto"
# base_url = "https://api.openai.com/v1"     # API endpoint (change for Azure, etc.)
# alias = "openai"
# [providers.openai.models."gpt-5.3"]
# [providers.openai.models."gpt-5.2"]

# ── Google Gemini ─────────────────────────────────────────────
# [providers.gemini]
# enabled = true
# api_key = "..."                             # Or set GEMINI_API_KEY / GOOGLE_API_KEY env var
# fetch_models = true
# base_url = "https://generativelanguage.googleapis.com/v1beta/openai"
# alias = "gemini"
# [providers.gemini.models."gemini-2.5-flash"]
# [providers.gemini.models."gemini-2.5-pro"]

# ── DeepInfra ─────────────────────────────────────────────────
# [providers.deepinfra]
# enabled = true
# api_key = "..."                             # Or set DEEPINFRA_API_KEY env var
# base_url = "https://api.deepinfra.com/v1/openai"
# alias = "deepinfra"
# [providers.deepinfra.models."meta-llama/Llama-4-Maverick-17B-128E-Instruct"]

# ── xAI (Grok) ────────────────────────────────────────────────
# [providers.xai]
# enabled = true
# api_key = "..."                             # Or set XAI_API_KEY env var
# alias = "xai"
# [providers.xai.models."grok-3-mini"]

# ── OpenRouter (multi-provider gateway) ───────────────────────
# [providers.openrouter]
# enabled = true
# api_key = "..."                             # Or set OPENROUTER_API_KEY env var
# base_url = "https://openrouter.ai/api/v1"
# [providers.openrouter.models."anthropic/claude-3.5-sonnet"]

# ── Moonshot (Kimi) ─────────────────────────────────────────
# [providers.moonshot]
# enabled = true
# api_key = "..."                             # Or set MOONSHOT_API_KEY env var
# base_url = "https://api.moonshot.ai/v1"
# alias = "moonshot"
# [providers.moonshot.models."kimi-k2.5"]

# ══════════════════════════════════════════════════════════════════════════════
# COMPLETE MODEL METADATA EXAMPLE
# ══════════════════════════════════════════════════════════════════════════════
# The same table format carries complete metadata when config supplies it.
#
# [providers.custom-ai-0xff-dad]
# enabled = true
# base_url = "https://ai.0xff.dad/v1"
# wire_api = "responses"
#
# [providers.custom-ai-0xff-dad.models."Combos/cx/gpt-sol"]
# context_length = 400000
# max_input_tokens = 272000
# max_output_tokens = 128000
# input_modalities = ["text", "image", "audio", "file"]
# output_modalities = ["text"]
# tool_calling = true
# streaming = true
# zeroDataRetentionEnabled = true
#
# [providers.custom-ai-0xff-dad.models."Combos/cx/gpt-sol".reasoning]
# supported_efforts = ["none", "minimal", "low", "medium", "high", "xhigh"]
# summary = "detailed"
# include = ["reasoning.encrypted_content"]

# ══════════════════════════════════════════════════════════════════════════════
# CHAT SETTINGS
# ══════════════════════════════════════════════════════════════════════════════

# [chat]
# auto_title = true                   # Auto-generate session title after first exchange
# message_queue_mode = "followup"   # How to handle messages during an active agent run:
                                    #   "followup" - Queue messages, replay one-by-one after run
                                    #   "collect"  - Buffer messages, concatenate as single message
# prompt_memory_mode = "live-reload"  # How MEMORY.md reaches the prompt:
                                      #   "live-reload"            - Re-read MEMORY.md before each turn
                                      #   "frozen-at-session-start" - Freeze the first MEMORY.md snapshot per session
# workspace_file_max_chars = 32000  # Optional: per-file prompt cap for AGENTS.md / TOOLS.md before truncation.
# priority_models = ["claude-opus-4-5", "gpt-5.2", "gemini-3-flash"]  # Optional cross-provider selector order

# ══════════════════════════════════════════════════════════════════════════════
# AUXILIARY MODELS
# ══════════════════════════════════════════════════════════════════════════════
# Route side tasks to cheaper/faster models while keeping the main session on a
# more capable model. Falls back to the session's primary provider when unset.
#
# [auxiliary]
# title_generation = "openrouter/google/gemini-2.5-flash"  # Model for session titles
# vision = "openrouter/google/gemini-2.5-flash"            # Model for vision/image tasks

# ══════════════════════════════════════════════════════════════════════════════
# SUB-AGENT SPAWN PRESETS
# ══════════════════════════════════════════════════════════════════════════════
# Configure reusable presets for agents and sub-agents spawned via the
# `spawn_agent` tool.
#
# Runtime fields like `timeout_secs` and `max_iterations` apply to matching
# direct agent sessions and spawned sub-agents. Direct sessions use global
# `[tools]` values as fallbacks when a preset omits them. Spawned sub-agents
# preserve no-timeout behavior unless the preset sets `timeout_secs`.
#
# ⚠️  SCOPE: `tools.allow` / `tools.deny` under a preset do NOT filter tools
# for the main agent session. To allow/deny tools for the main session, use
# the `[tools.policy]` section further down this file.
#
# [agents]
# default_preset = "research"      # Sub-agent preset used when spawn_agent.preset is omitted
#
# Built-in agent presets (research, coder, reviewer, qa, ux, docs, coordinator)
# live in defaults.toml. Uncomment and modify below to override a preset,
# or add your own custom presets.
#
# [agents.presets.research]
# identity.name = "Researcher"
# identity.theme = "thorough, skeptical, and evidence-oriented"
# tools.preload = ["Read", "Glob", "Grep"] # Schemas sent immediately when tools.registry_mode = "lazy"; allow/deny still apply
# system_prompt_suffix = "..."
# max_iterations = 16
# max_tool_result_bytes = 100000   # Per-agent override of tools.max_tool_result_bytes
# # Optional drift-resistant per-turn controls for spawned/preset agents:
# # [agents.presets.research.tool_controls]
# # active_tools = ["classify_destination"]
# # [agents.presets.research.tool_controls.tool_choice]
# # type = "tool"  # auto | any | none | tool
# # name = "classify_destination"
#
# ── Per-agent capability boundaries ──────────────────────────────────────────
# Each agent can be scoped to specific MCP servers and skills.
# Assign agents to channels via `agent_id` in the channel account config.
#
# Example: restricted agent for kids (no MCP, no network, limited skills):
# [agents.presets.kids]
# model = "anthropic/claude-haiku-4-5-20251001"
# [agents.presets.kids.mcp]
# allow_servers = []                # No MCP tools at all
# [agents.presets.kids.skills]
# deny = ["gaming", "social-media"] # Block specific skill categories
#
# Example: full-access agent for parents:
# [agents.presets.admin]
# [agents.presets.admin.mcp]
# allow_servers = ["github", "home-assistant", "memory"]
# ══════════════════════════════════════════════════════════════════════════════
# SANDBOX
# ══════════════════════════════════════════════════════════════════════════════
# Agent sessions and command execution can run inside isolated containers for security.

# [sandbox]
# mode = "On"                       # "On" | "Off"; global for every session
# scope = "session"                 # "session" | "agent" | "shared"
# backend = "auto"                  # "auto" | "docker" | "podman" | "apple-container" | "wasm"
# image = "custom-image:tag"        # Custom container image (default: auto-built)
# network = "bridge"                # Docker/Podman network passed as --network=<name>
# workspace_sysmount = "ro"         # "ro" | "rw" (rootfs + cap-drop/no-new-privileges hardening)
# host_data_dir = "/host/chelix-data" # Host path for Chelix data when running Chelix inside Docker
# home_persistence = "shared"       # "off" | "session" | "shared"
# shared_home_dir = "sandbox/home"  # Directory for shared /home/sandbox persistence (relative to data_dir)
# gpus = "all"                      # GPU passthrough: "all", "device=0", "device=0,1"
# packages = []                     # Packages installed in sandbox containers (default list lives in defaults.toml)
# wasm_fuel_limit = 1000000000      # Optional WASM fuel limit
# wasm_epoch_interval_ms = 100      # Optional WASM epoch interruption interval

# [sandbox.resource_limits]
# memory_limit = "512M"             # Memory limit (e.g., "512M", "1G")
# cpu_quota = 1.0                   # CPU quota; Docker/Podman default to one core when unset
# pids_max = 100                    # Maximum number of processes

# [sandbox.wasm_tool_limits]
# default_memory = 16777216         # Default WASM tool memory limit
# default_fuel = 1000000            # Default WASM tool fuel limit

# [sandbox.tools_policy]
# allow = []                        # Tools allowed only when this sandbox policy layer applies
# deny = []                         # Tools denied only when this sandbox policy layer applies

# data_dir is always mounted read-write at the identical guest path.
# Add optional non-secret mounts as array entries:
# [[sandbox.mounts]]
# host = "/srv/reference"            # Host path to mount
# guest = "/mnt/reference"           # Absolute path inside the sandbox
# mode = "ro"                        # "ro" | "rw"

# ══════════════════════════════════════════════════════════════════════════════
# SESSION MODES
# ══════════════════════════════════════════════════════════════════════════════
# Modes are temporary per-session prompt overlays selected with `/mode`.
# They do not create chat agents, do not affect sub-agent presets, and do not
# change an agent's identity or memory. Built-ins include concise, technical,
# creative, teacher, plan, build, review, research, and elevated.
#
# [modes.presets.concise]
# name = "Concise"
# description = "short direct answers"
# prompt = "Keep answers short, concrete, and caveat-light unless the user asks for detail."
#
# [modes.presets.incident]
# name = "Incident"
# description = "production incident response"
# prompt = "Prioritize impact, timeline, mitigation, rollback, logs, and clear status updates."

# ══════════════════════════════════════════════════════════════════════════════
# TOOLS
# ══════════════════════════════════════════════════════════════════════════════

# [tools]
# agent_timeout_secs = 600          # Max seconds for an agent run (0 = no timeout)
# agent_max_iterations = 25         # Max LLM/tool loop iterations before stopping
# agent_max_auto_continues = 2      # Auto-continue nudges when model stops mid-task (0 = off)
# agent_auto_continue_min_tool_calls = 3  # Min tool calls before auto-continue can trigger
# max_tool_result_bytes = 50000     # Max in-context bytes per tool result before truncation (50KB).
#                                   # Full outputs are always persisted under
#                                   # <data_dir>/sessions/tool-results/<session>/<call>/ and truncated
#                                   # results point at the persisted file. Override per agent with
#                                   # `max_tool_result_bytes` on the agent preset.
# registry_mode = "full"            # "full" = all schemas every turn, "lazy" = catalog + on-demand get_tool schema fetch
# agent_loop_detector_window = 2    # Fire after N model rounds repeat an equivalent failure

# ── Maps ─────────────────────────────────────────────────────────────────────

# [tools.maps]
# provider = "google_maps"          # "google_maps" | "apple_maps" | "openstreetmap"

# ── Native filesystem tools (Read/Write/Edit/MultiEdit/Glob/Grep) ─────────────
# All fields are optional. Defaults are conservative — the fs tools work
# out of the box with no configuration.

# [tools.fs]
# workspace_root = "/home/user/projects/my-app"  # Default search root for Glob/Grep
# allow_paths = []                  # Absolute path globs the fs tools are allowed to access
# deny_paths = []                   # Absolute path globs the fs tools must refuse
# track_reads = false               # Record per-session Read history
# must_read_before_write = false    # Refuse Write/Edit targeting unread files (needs track_reads)
# require_approval = true           # Pause Write/Edit for operator approval
# max_read_bytes = 10485760         # Max bytes per Read (10 MB)
# binary_policy = "reject"          # "reject" or "base64"
# respect_gitignore = true          # Skip .gitignored files in Glob/Grep

# ── Command Execution ─────────────────────────────────────────────────────────

# [tools.execute_command]
# default_timeout_secs = 30         # Default timeout for commands
# max_output_bytes = 204800         # Max command output bytes (200KB)
# approval_mode = "on-miss"         # "always" | "on-miss" | "never"
# security_level = "allowlist"      # "permissive" | "allowlist" | "strict"
# allowlist = []                    # Command patterns to allow. Example: ["git *", "npm *"]
# host = "local"                    # "local" | "node" | "ssh"
# node = "mac-mini"                 # Default node when host = "node"
# ssh_target = "deploy@box"         # SSH target when host = "ssh"

# ── Tool Policy ───────────────────────────────────────────────────────────────
# Control which tools the agent can use. Policies are layered (later wins for
# allow, deny always accumulates across layers):
#
#   1. Global        — [tools.policy]
#   2. Per-provider  — [providers.<name>.policy]
#   3. Per-agent     — [agents.presets.<id>.tools]
#   4. Per-channel   — [channels.<type>.<account>.tools.groups.<chat_type>]
#   5. Per-sender    — [...groups.<chat_type>.by_sender.<sender_id>]
#   6. Sandbox       — [sandbox.tools_policy]

# [tools.policy]
# allow = []                        # Tools to always allow (e.g., ["execute_command", "web_fetch"])
# deny = []                         # Tools to always deny (e.g., ["browser"])

# ── Web Search ────────────────────────────────────────────────────────────────

# [tools.web.search]
# enabled = true                    # Enable web search tool
# provider = "brave"                # "brave" or "perplexity"
# max_results = 5                   # Number of results to return (1-10)
# timeout_seconds = 30              # HTTP request timeout
# cache_ttl_minutes = 15            # Cache results (0 = no cache)
# duckduckgo_fallback = false       # Enable DDG fallback without API keys
# api_key = "..."                   # Brave API key (or set BRAVE_API_KEY env var)

# [tools.web.search.perplexity]
# api_key = "..."                   # Or set PERPLEXITY_API_KEY env var
# model = "sonar"                   # Perplexity model to use

# ── Web Fetch ─────────────────────────────────────────────────────────────────

# [tools.web.fetch]
# enabled = true                    # Enable web fetch tool
# max_chars = 50000                 # Max characters to return
# timeout_seconds = 30              # HTTP request timeout
# cache_ttl_minutes = 15            # Cache fetched pages (0 = no cache)
# max_redirects = 3                 # Maximum HTTP redirects
# readability = true                # Use readability extraction for HTML
# ssrf_allowlist = ["172.22.0.0/16"] # CIDR ranges exempt from SSRF blocking

# ── Firecrawl (API-based web scraping) ────────────────────────────────────────

# [tools.web.firecrawl]
# enabled = false
# api_key = "fc-..."                # Or set FIRECRAWL_API_KEY env var
# base_url = "https://api.firecrawl.dev"

# ── Browser Automation ────────────────────────────────────────────────────────

# [tools.browser]
# enabled = true                    # Enable browser tool
# headless = true                   # Run without visible window
# viewport_width = 2560             # Default viewport width in pixels
# viewport_height = 1440            # Default viewport height
# device_scale_factor = 2.0         # HiDPI/Retina scaling
# max_instances = 3                 # Maximum concurrent browser instances
# idle_timeout_secs = 300           # Close idle browsers after this many seconds
# sandbox = false                   # Run browser in container for isolation
# allowed_domains = []              # Domain restrictions (empty = all allowed)
# chrome_path = "/path/to/chrome"   # Custom Chrome binary path
# obscura_path = "/path/to/obscura" # Custom Obscura binary path for browser = "obscura"
# lightpanda_path = "/path/to/lightpanda" # Custom Lightpanda binary path for browser = "lightpanda"

# ══════════════════════════════════════════════════════════════════════════════
# SKILLS
# ══════════════════════════════════════════════════════════════════════════════

# [skills]
# enabled = true                    # Enable skills system
# search_paths = []                 # Additional directories to search for skills
# auto_load = []                    # Skills to always load
# disabled_bundled_categories = []   # Bundled skill categories to disable
# disabled_bundled_skills = []       # Individual bundled skills to disable by name

# ══════════════════════════════════════════════════════════════════════════════
# MCP SERVERS
# ══════════════════════════════════════════════════════════════════════════════
# Model Context Protocol servers provide additional tools and capabilities.
# See https://modelcontextprotocol.io for available servers.

# [mcp]
# request_timeout_secs = 30         # Default timeout for MCP requests

# [mcp.servers.server-name]
# command = "npx"                   # Command to run (for stdio transport)
# args = ["-y", "@package/name"]    # Command arguments
# env = {{ KEY = "value" }}           # Environment variables
# transport = "stdio"               # "stdio" | "sse" | "streamable-http"

# [mcp.servers.server-name.oauth]
# client_id = "your-client-id"       # Manual OAuth client ID
# client_secret = "your-secret"      # Optional secret for token exchange
# auth_url = "https://auth.example.com/authorize"
# token_url = "https://auth.example.com/token"
# scopes = ["mcp:read"]

# ══════════════════════════════════════════════════════════════════════════════
# METRICS
# ══════════════════════════════════════════════════════════════════════════════

# [metrics]
# enabled = true                    # Enable metrics collection
# prometheus_endpoint = true        # Expose /metrics endpoint

# ══════════════════════════════════════════════════════════════════════════════
# CRON
# ══════════════════════════════════════════════════════════════════════════════

# [cron]
# rate_limit_max = 10
# rate_limit_window_secs = 60
# session_retention_days = 7

# ══════════════════════════════════════════════════════════════════════════════
# HEARTBEAT
# ══════════════════════════════════════════════════════════════════════════════

# [heartbeat]
# enabled = true                    # Enable periodic heartbeats
# every = "30m"                     # Interval (e.g., "30m", "1h", "6h")
# ack_max_chars = 300               # Max characters for acknowledgment reply
# deliver = false                   # Deliver heartbeat replies to a channel
# wake_cooldown = "5m"              # Min duration between command-triggered heartbeat wakes (0 to disable)

# [heartbeat.active_hours]
# start = "08:00"
# end = "24:00"
# timezone = "local"                # "local" or IANA name like "Europe/Paris"

# ══════════════════════════════════════════════════════════════════════════════
# VOICE
# ══════════════════════════════════════════════════════════════════════════════

# [voice.tts]
# enabled = true
# providers = []                        # UI allowlist (empty = show all)

# Voice personas — named voice identities injected into TTS calls.
# Personas are managed via the web UI (Settings > Voice > Voice Personas)
# and stored in the database. Example via TOML for reference:
#
# Configure personas in the web UI, or use the RPC API:
#   voice.personas.create  — create a new persona
#   voice.personas.list    — list all personas
#   voice.personas.set_active — activate a persona
#
# Providers that support instructions (OpenAI gpt-4o-mini-tts) will receive
# the persona's profile/style/accent as voice direction. Other providers
# use the persona's provider-specific bindings (voice_id, model overrides).

# [voice.stt]
# enabled = true
# providers = []                        # UI allowlist (empty = show all)

# [voice.stt.whisper_local]
# endpoint = "http://localhost:8080"    # OpenAI-compatible transcription server
# model = "whisper-large-v3"            # Model name (server-specific)
# language = "en"                       # Optional ISO 639-1 hint

# ══════════════════════════════════════════════════════════════════════════════
# MEMORY / EMBEDDINGS
# ══════════════════════════════════════════════════════════════════════════════

# [memory]
# style = "hybrid"                  # "hybrid" | "prompt-only" | "search-only" | "off"
# agent_write_mode = "hybrid"       # "hybrid" | "prompt-only" | "search-only" | "off"
# backend = "builtin"               # "builtin" | "qmd"
# provider = "auto"                 # "local" (managed sidecar) | "openai" | "custom"
# model = "/path/to/model.gguf"     # Local GGUF path or remote provider model name
# base_url = "/path/to/model-cache" # Cache directory when provider = "local"

# ══════════════════════════════════════════════════════════════════════════════
# PHONE (Telephony Providers)
# ══════════════════════════════════════════════════════════════════════════════
# Configure telephony providers for making and receiving phone calls.
# Provider credentials are stored securely via the web UI (Settings > Phone).

# [phone]
# enabled = false                           # Enable phone calls globally
# provider = "twilio"                       # Active provider
# inbound_policy = "disabled"               # disabled | allowlist | open
# allowlist = []                            # Allowed inbound callers (E.164)
# max_duration_secs = 3600                  # Max call duration (1 hour)

# [phone.twilio]
# from_number = "+15551234567"              # Your Twilio phone number (E.164)
# webhook_url = "https://your-domain.com"   # Public URL for Twilio callbacks

# [phone.telnyx]
# from_number = "+15551234567"              # Your Telnyx phone number (E.164)
# webhook_url = "https://your-domain.com"   # Public URL for Telnyx callbacks

# [phone.plivo]
# from_number = "+15551234567"              # Your Plivo phone number (E.164)
# webhook_url = "https://your-domain.com"   # Public URL for Plivo callbacks

# ══════════════════════════════════════════════════════════════════════════════
# EXTERNAL AGENTS
# ══════════════════════════════════════════════════════════════════════════════
# Connect Chelix chat sessions to external CLI coding agents.
# Codex and ACP use persistent JSON-RPC sessions; Claude Code uses print-mode
# resume when the CLI returns a session_id.
# Chelix acts as orchestrator; the CLI agent owns its own context window.

[external_agents]
# enabled = false                   # Enable external agent bridge

# Per-agent configuration (key = agent kind)
# [external_agents.agents.claude-code]
# binary = "claude"                 # Override binary path (default: look up on $PATH)
# args = ["-p", "--output-format", "json"]
# working_dir = "."                 # Override working directory
# timeout_secs = 300                # Session timeout
# use_tmux = false                  # Force tmux backend (vs direct PTY)
# [external_agents.agents.claude-code.env]
# ANTHROPIC_API_KEY = "sk-..."      # Extra env vars for this agent

# [external_agents.agents.codex]
# binary = "codex"
# args = ["app-server"]

# [external_agents.agents.acp]
# binary = "/path/to/acp-agent"
# args = []

# [external_agents.agents.opencode]
# binary = "opencode"
# use_tmux = true                   # opencode requires tmux (TUI app)

# ══════════════════════════════════════════════════════════════════════════════
# CHANNELS
# ══════════════════════════════════════════════════════════════════════════════
# External messaging integrations.
# Note: channels added in the web UI are stored in data_dir()/chelix.db,
# not in this file. Keep channel config here only for manual TOML management.

# [channels]
# offered = ["telegram", "whatsapp", "msteams", "discord", "slack", "matrix", "nostr", "signal"]

# See docs or defaults.toml for full channel configuration examples
# (WhatsApp, Telegram, Teams, Discord, Slack, Matrix, Nostr, Signal).

# ══════════════════════════════════════════════════════════════════════════════
# HOOKS
# ══════════════════════════════════════════════════════════════════════════════

# [hooks]
# [[hooks.hooks]]
# name = "my-hook"
# command = "/path/to/handler.sh"
# events = ["BeforeToolCall", "AfterToolCall"]
# timeout = 10

# ══════════════════════════════════════════════════════════════════════════════
# ENVIRONMENT VARIABLES
# ══════════════════════════════════════════════════════════════════════════════
# Variables injected into the Chelix process at startup.

# [env]
# BRAVE_API_KEY = "..."
# OPENROUTER_API_KEY = "sk-or-..."
"##
    )
}
