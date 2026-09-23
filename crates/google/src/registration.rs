use {
    crate::tool::GoogleSearchTool, chelix_agents::tool_registry::ToolRegistry,
    chelix_config::schema::GoogleConfig, std::sync::Arc,
};

/// Register one Google client shared by all agent sessions.
pub fn register_tools(registry: &mut ToolRegistry, config: &GoogleConfig) {
    registry.register(Box::new(GoogleSearchTool::new(Arc::new(config.clone()))));
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn registers_search_tool() {
        let mut registry = ToolRegistry::new();
        register_tools(&mut registry, &GoogleConfig::default());
        assert_eq!(registry.list_catalog()[0].name, "google_search");
    }
}
