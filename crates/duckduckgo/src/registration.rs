//! Registration of `duckduckgo_search` in an agent tool registry.

use std::sync::Arc;

use {chelix_agents::tool_registry::ToolRegistry, chelix_config::schema::DuckDuckGoConfig};

use crate::{client::DuckDuckGoClient, tool::DuckDuckGoSearchTool};

/// Register `duckduckgo_search` against one shared client and request queue.
pub fn register_tools(
    registry: &mut ToolRegistry,
    config: &DuckDuckGoConfig,
) -> anyhow::Result<()> {
    let client = Arc::new(DuckDuckGoClient::new(config.request_timeout_secs)?);
    registry.register(Box::new(DuckDuckGoSearchTool::new(client)));
    Ok(())
}
