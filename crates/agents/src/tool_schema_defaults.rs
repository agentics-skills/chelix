//! Live defaults used while constructing tool schemas.

use chelix_config_service::ConfigService;

/// Current agent and command defaults interpolated into tool schemas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolSchemaDefaults {
    pub max_tool_result_bytes: usize,
    pub execute_command_default_timeout_secs: u64,
}

impl ToolSchemaDefaults {
    pub async fn read(config_service: &ConfigService, agent_id: &str) -> anyhow::Result<Self> {
        let id = agent_id.to_owned();
        let defaults = config_service
            .read(move |config| {
                config.agent_runtime_limits(&id).map(|limits| Self {
                    max_tool_result_bytes: limits.max_tool_result_bytes,
                    execute_command_default_timeout_secs: config
                        .tools
                        .execute_command
                        .default_timeout_secs,
                })
            })
            .await??;
        Ok(defaults)
    }
}
