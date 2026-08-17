//! Registration of the `context7_*` tool set in an agent tool registry.

use std::sync::Arc;

use {chelix_agents::tool_registry::ToolRegistry, chelix_config::schema::Context7Config};

use crate::{
    client::Context7Client,
    tools::{Context7GetLibraryDocsTool, Context7ResolveLibraryIdTool},
};

/// Register every `context7_*` tool against one shared client.
pub fn register_tools(registry: &mut ToolRegistry, config: &Context7Config) {
    let client = Arc::new(Context7Client::new(
        config.token.clone(),
        config.request_timeout_secs,
    ));
    registry.register(Box::new(Context7ResolveLibraryIdTool::new(Arc::clone(
        &client,
    ))));
    registry.register(Box::new(Context7GetLibraryDocsTool::new(client)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registers_the_complete_tool_set() {
        let mut registry = ToolRegistry::new();
        register_tools(&mut registry, &Context7Config::default());

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
