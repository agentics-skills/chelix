//! Registration of `linkup_search` in an agent tool registry.

use std::sync::Arc;

use {chelix_agents::tool_registry::ToolRegistry, chelix_config_service::ConfigService};

use crate::{client::LinkupClient, tool::LinkupSearchTool};

/// Register `linkup_search` against one shared client.
pub fn register_tools(registry: &mut ToolRegistry, config_service: ConfigService) {
    let client = Arc::new(LinkupClient::new(config_service));
    registry.register(Box::new(LinkupSearchTool::new(client)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn registers_the_complete_tool_set() {
        let mut registry = ToolRegistry::new();
        let fixture =
            chelix_agents::test_support::ConfigFixture::new(chelix_config::ChelixConfig::default())
                .await
                .unwrap_or_else(|error| panic!("config fixture failed: {error}"));
        register_tools(&mut registry, fixture.service.clone());

        let names: Vec<String> = registry
            .list_catalog()
            .into_iter()
            .map(|entry| entry.name)
            .collect();
        assert_eq!(names, vec!["linkup_search".to_string()]);
    }
}
