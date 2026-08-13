//! Configuration loading, validation, and environment substitution.
//!
//! Config files: `chelix.toml`, `chelix.yaml`, or `chelix.json`
//! Searched in `./` then `~/.config/chelix/`.
//!
//! Supports `${ENV_VAR}` substitution in all string values.

pub mod container_mounts;
pub mod defaults;
pub mod env_subst;
pub mod error;
pub mod loader;
pub mod migrate;
pub mod provider_env;
pub mod schema;
pub mod template;
mod tools_config_source;
pub mod validate;
pub mod version;

pub use {tools_config_source::ToolsConfigSource, version::VERSION};

pub use {
    error::{Error, Result},
    loader::{
        DEFAULT_SOUL, LoadedWorkspaceMarkdown, WorkspaceMarkdownSource, agent_workspace_dir,
        agents_path, apply_env_overrides, boot_path, clear_config_dir, clear_data_dir,
        clear_share_dir, compact_config, config_dir, data_dir, discover_and_load,
        extract_yaml_frontmatter, find_or_default_config_path, guidelines_path, heartbeat_path,
        home_dir, initialize_config, load_agents_md, load_agents_md_for_agent, load_boot_md,
        load_boot_md_for_agent, load_guidelines_md, load_guidelines_md_for_agent,
        load_heartbeat_md, load_memory_md, load_memory_md_for_agent,
        load_memory_md_for_agent_with_source, load_soul_for_agent, load_subagent_prompt_for_agent,
        load_tools_md, load_tools_md_for_agent, load_user, memory_path,
        normalize_workspace_markdown_content, resolve_identity, resolve_identity_from_config,
        resolve_user_profile, resolve_user_profile_from_config, resubstitute_config, save_config,
        save_raw_config, save_soul_for_agent, save_subagent_prompt_for_agent, save_user,
        save_user_with_mode, set_config_dir, set_data_dir, set_share_dir, share_dir, tools_path,
        update_config, update_config_checked, user_path,
    },
    provider_env::{
        GenericProviderEnv, env_value_with_overrides, generic_provider_api_key_from_env,
        generic_provider_env, generic_provider_env_source_for_provider, normalize_provider_name,
    },
    schema::{
        AgentConfig, AgentMcpPolicy, AgentMemoryConfig, AgentMemoryWriteMode,
        AgentRuntimeLimitSource, AgentRuntimeLimits, AgentSkillPolicy, AgentToolPolicy,
        AgentsConfig, ApprovalMode, AuthConfig, CacheRetention, CalDavAccountConfig, CalDavConfig,
        ChannelToolPolicyOverride, ChannelsConfig, ChatConfig, ChelixConfig, CodeIndexTomlConfig,
        GeoLocation, GroupToolPolicy, HeartbeatConfig, HomeAssistantAccountConfig,
        HomeAssistantConfig, MemoryBackend, MemoryCitationsMode, MemoryProvider, MemoryScope,
        MemorySearchMergeStrategy, MemoryStyle, MessageQueueMode, PromptMemoryMode,
        ResolvedIdentity, SessionAccessPolicyConfig, SessionExportMode, Timezone, ToolMode,
        ToolPolicyConfig, ToolRegistryMode, UserProfile, UserProfileWriteMode, VoiceConfig,
        VoiceElevenLabsConfig, VoiceOpenAiConfig, VoiceSttConfig, VoiceSttProvider, VoiceTtsConfig,
        VoiceTtsProvider, VoiceWhisperConfig, VoiceWhisperLocalConfig, WireApi, parse_byte_size,
        validate_agent_id,
    },
    validate::{Diagnostic, Severity, ValidationResult},
};
