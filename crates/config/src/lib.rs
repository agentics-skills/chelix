//! Configuration schema, directory resolution, workspace files, and TOML codec.
//!
//! The config directory is the programmatic override, an absolute
//! `CHELIX_CONFIG_DIR`, or `~/.config/chelix/`.
//!
//! [`parse_raw_config`] deserializes TOML and rejects unknown fields.
//! [`parse_config_source`] does that after two-pass `${VAR}` substitution from
//! the document's `[env]` table.

pub mod container_mounts;
pub mod env_subst;
pub mod error;
mod llm_assignment;
pub mod loader;
pub mod migrate;
pub mod provider_env;
pub mod schema;
pub mod template;
pub mod toml_document;
pub mod validate;
pub mod version;

pub use version::VERSION;

pub use {
    error::{Error, Result},
    loader::{
        DEFAULT_SOUL, LoadedWorkspaceMarkdown, WorkspaceMarkdownSource, agent_workspace_dir,
        agents_path, boot_path, clear_config_dir, clear_data_dir, clear_share_dir, config_dir,
        data_dir, extract_yaml_frontmatter, guidelines_path, home_dir, load_agents_md,
        load_agents_md_for_agent, load_boot_md, load_boot_md_for_agent, load_guidelines_md,
        load_guidelines_md_for_agent, load_memory_md, load_memory_md_for_agent,
        load_memory_md_for_agent_with_source, load_soul_for_agent, load_subagent_prompt_for_agent,
        load_tools_md, load_tools_md_for_agent, load_user, memory_path,
        normalize_workspace_markdown_content, parse_config_source, parse_raw_config,
        resolve_user_profile_from_config, resubstitute_config, save_soul_for_agent,
        save_subagent_prompt_for_agent, save_user, save_user_with_mode, set_config_dir,
        set_data_dir, set_share_dir, share_dir, tools_path, user_path,
    },
    provider_env::normalize_provider_name,
    schema::{
        AgentConfig, AgentMcpPolicy, AgentMemoryWriteMode, AgentRuntimeLimitSource,
        AgentRuntimeLimits, AgentSkillPolicy, AgentToolPolicy, AgentsConfig, AgentsConfigState,
        AgentsConfigStateError, ApprovalMode, AuthConfig, ChannelToolPolicyOverride,
        ChannelsConfig, ChatConfig, ChelixConfig, CodeIndexTomlConfig, EmbeddingEndpoint,
        GeoLocation, GroupToolPolicy, MemoryCitationsMode, MemorySearchMergeStrategy, MemoryStyle,
        PromptMemoryMode, ResolvedIdentity, SessionAccessPolicyConfig, TerminalSizeConfig,
        Timezone, ToolMode, ToolPolicyConfig, ToolRegistryMode, UserProfile, UserProfileWriteMode,
        VoiceConfig, VoiceElevenLabsConfig, VoiceOpenAiConfig, VoiceSttConfig, VoiceSttProvider,
        VoiceTtsConfig, VoiceTtsProvider, VoiceWhisperConfig, VoiceWhisperLocalConfig, WireApi,
        parse_byte_size, resolve_instance_slug, validate_agent_id,
    },
    validate::{Diagnostic, DiagnosticCategory, Severity, ValidationResult},
};
