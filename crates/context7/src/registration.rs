//! Registration of the `context7_*` tool set in an agent tool registry.

use std::sync::Arc;

use {chelix_agents::tool_registry::ToolRegistry, chelix_config_service::ConfigService};

use crate::{
    client::Context7Client,
    tools::{Context7GetLibraryDocsTool, Context7ResolveLibraryIdTool},
};

/// Register every `context7_*` tool against one shared client.
pub fn register_tools(registry: &mut ToolRegistry, config_service: ConfigService) {
    let client = Arc::new(Context7Client::new(config_service));
    registry.register(Box::new(Context7ResolveLibraryIdTool::new(Arc::clone(
        &client,
    ))));
    registry.register(Box::new(Context7GetLibraryDocsTool::new(client)));
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
        assert_eq!(names, vec![
            "context7_get_library_docs".to_string(),
            "context7_resolve_library_id".to_string(),
        ]);
    }
}
