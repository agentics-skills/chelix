use {
    crate::tool::FeloSearchTool, chelix_agents::tool_registry::ToolRegistry,
    chelix_config::schema::FeloConfig, std::sync::Arc,
};

/// Register one Felo client shared by all agent sessions.
pub fn register_tools(registry: &mut ToolRegistry, config: &FeloConfig) {
    registry.register(Box::new(FeloSearchTool::new(Arc::new(config.clone()))));
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn registers_search_tool() {
        let mut registry = ToolRegistry::new();
        register_tools(&mut registry, &FeloConfig::default());
        assert_eq!(registry.list_catalog()[0].name, "felo_search");
    }
}
