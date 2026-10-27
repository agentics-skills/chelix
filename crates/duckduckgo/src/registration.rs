//! Registration of `duckduckgo_search` in an agent tool registry.

use std::sync::Arc;

use {chelix_agents::tool_registry::ToolRegistry, chelix_config_service::ConfigService};

use crate::{client::DuckDuckGoClient, tool::DuckDuckGoSearchTool};

/// Register `duckduckgo_search` against one shared client and request queue.
pub fn register_tools(registry: &mut ToolRegistry, config: ConfigService) -> anyhow::Result<()> {
    let client = Arc::new(DuckDuckGoClient::new(config)?);
    registry.register(Box::new(DuckDuckGoSearchTool::new(client)));
    Ok(())
}
