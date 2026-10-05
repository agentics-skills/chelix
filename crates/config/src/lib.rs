//! Configuration loading, validation, and environment substitution.
//!
//! Config files: `chelix.toml`, `chelix.yaml`, or `chelix.json`
//! Searched in `./` then `~/.config/chelix/`.
//!
//! Supports `${ENV_VAR}` substitution in all string values.

pub mod container_mounts;
pub mod env_subst;
pub mod error;
mod llm_assignment;
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
        DEFAULT_SOUL, LoadedWorkspaceMarkdown, OpenAiCompatibleProviderTomlResult,
        OpenAiCompatibleProviderTomlUpdate, WorkspaceMarkdownSource, agent_workspace_dir,
        agents_path, apply_env_overrides, boot_path, clear_config_dir, clear_data_dir,
        clear_provider_api_key, clear_share_dir, config_dir, data_dir,
        delete_openai_compatible_provider_toml, discover_and_load, extract_yaml_frontmatter,
        find_or_default_config_path, guidelines_path, heartbeat_path, home_dir, initialize_config,
        load_agents_md, load_agents_md_for_agent, load_boot_md, load_boot_md_for_agent,
        load_guidelines_md, load_guidelines_md_for_agent, load_heartbeat_md,
        load_layered_config_candidate, load_memory_md, load_memory_md_for_agent,
        load_memory_md_for_agent_with_source, load_soul_for_agent, load_subagent_prompt_for_agent,
        load_tools_md, load_tools_md_for_agent, load_user, memory_path,
        normalize_workspace_markdown_content, resolve_user_profile,
        resolve_user_profile_from_config, resubstitute_config, save_config, save_raw_config,
        save_soul_for_agent, save_subagent_prompt_for_agent, save_user, save_user_with_mode,
        set_config_dir, set_config_strings, set_data_dir, set_model_enabled_in_toml,
        set_provider_enabled_flag, set_share_dir, share_dir, tools_path, update_config,
        update_config_checked, update_provider_model_toml, upsert_openai_compatible_provider_toml,
        user_path, write_provider_api_key,
    },
    provider_env::normalize_provider_name,
    schema::{
        AgentConfig, AgentMcpPolicy, AgentMemoryWriteMode, AgentRuntimeLimitSource,
        AgentRuntimeLimits, AgentSkillPolicy, AgentToolPolicy, AgentsConfig, AgentsConfigState,
        AgentsConfigStateError, ApprovalMode, AuthConfig, CalDavAccountConfig, CalDavConfig,
        ChannelToolPolicyOverride, ChannelsConfig, ChatConfig, ChelixConfig, CodeIndexTomlConfig,
        EmbeddingEndpoint, GeoLocation, GroupToolPolicy, HeartbeatConfig,
        HomeAssistantAccountConfig, HomeAssistantConfig, MemoryCitationsMode,
        MemorySearchMergeStrategy, MemoryStyle, PromptMemoryMode, ResolvedIdentity,
        SessionAccessPolicyConfig, TerminalSizeConfig, Timezone, ToolMode, ToolPolicyConfig,
        ToolRegistryMode, UserProfile, UserProfileWriteMode, VoiceConfig, VoiceElevenLabsConfig,
        VoiceOpenAiConfig, VoiceSttConfig, VoiceSttProvider, VoiceTtsConfig, VoiceTtsProvider,
        VoiceWhisperConfig, VoiceWhisperLocalConfig, WireApi, parse_byte_size,
        resolve_instance_slug, validate_agent_id,
    },
    validate::{Diagnostic, Severity, ValidationResult},
};
