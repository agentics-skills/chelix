use {
    crate::tool::FeloSearchTool, chelix_agents::tool_registry::ToolRegistry,
    chelix_config_service::ConfigService,
};

/// Register one Felo client shared by all agent sessions.
pub fn register_tools(registry: &mut ToolRegistry, config: ConfigService) {
    registry.register(Box::new(FeloSearchTool::new(config)));
}

#[cfg(test)]
mod tests {
    use {super::*, chelix_agents::test_support::ConfigFixture, chelix_config::ChelixConfig};

    #[tokio::test]
    async fn registers_search_tool() -> anyhow::Result<()> {
        let fixture = ConfigFixture::new(ChelixConfig::default()).await?;
        let mut registry = ToolRegistry::new();
        register_tools(&mut registry, fixture.service.clone());
        assert_eq!(registry.list_catalog()[0].name, "felo_search");
        Ok(())
    }
}
