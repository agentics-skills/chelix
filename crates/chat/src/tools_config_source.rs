//! Source used to resolve tools configuration for a new agent run.

use std::sync::Arc;

use {chelix_config::schema::ToolsConfig, chelix_config_service::ConfigService};

/// Source used to resolve tools configuration for a new agent run.
#[derive(Clone)]
pub enum ToolsConfigSource {
    /// Read `[tools]` from the shared configuration service for every new run.
    Service(ConfigService),
    /// Reuse an explicit immutable snapshot.
    Snapshot(Arc<ToolsConfig>),
}

impl std::fmt::Debug for ToolsConfigSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Service(_) => formatter.write_str("ToolsConfigSource::Service(..)"),
            Self::Snapshot(config) => formatter.debug_tuple("Snapshot").field(config).finish(),
        }
    }
}

impl ToolsConfigSource {
    #[must_use]
    pub fn snapshot(config: ToolsConfig) -> Self {
        Self::Snapshot(Arc::new(config))
    }

    pub async fn load(&self) -> chelix_config_service::Result<ToolsConfig> {
        match self {
            Self::Service(service) => service.read(|config| config.tools.clone()).await,
            Self::Snapshot(config) => Ok(config.as_ref().clone()),
        }
    }
}
