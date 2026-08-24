//! Registration of `linkup_search` in an agent tool registry.

use std::sync::Arc;

use {chelix_agents::tool_registry::ToolRegistry, chelix_config::schema::LinkupConfig};

use crate::{client::LinkupClient, tool::LinkupSearchTool};

/// Register `linkup_search` against one shared client.
pub fn register_tools(registry: &mut ToolRegistry, config: &LinkupConfig) {
    let client = Arc::new(LinkupClient::new(
        config.token.clone(),
        config.request_timeout_secs,
    ));
    registry.register(Box::new(LinkupSearchTool::new(client)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registers_the_complete_tool_set() {
        let mut registry = ToolRegistry::new();
        register_tools(&mut registry, &LinkupConfig::default());

        let names: Vec<String> = registry
            .list_catalog()
            .into_iter()
            .map(|entry| entry.name)
            .collect();
        assert_eq!(names, vec!["linkup_search".to_string()]);
    }
}
