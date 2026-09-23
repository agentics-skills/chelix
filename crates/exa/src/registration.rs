use {
    crate::tool::ExaSearchTool, chelix_agents::tool_registry::ToolRegistry,
    chelix_config::schema::ExaConfig, std::sync::Arc,
};

/// Register the Exa client once for all agent sessions.
pub fn register_tools(registry: &mut ToolRegistry, config: &ExaConfig) {
    registry.register(Box::new(ExaSearchTool::new(Arc::new(config.clone()))));
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn registers_search_tool() {
        let mut registry = ToolRegistry::new();
        register_tools(&mut registry, &ExaConfig::default());
        assert_eq!(registry.list_catalog()[0].name, "exa_search");
    }
}
