use std::sync::Arc;

use crate::{Result, schema::ToolsConfig};

/// Source used to resolve tools configuration for a new agent run.
#[derive(Debug, Clone)]
pub enum ToolsConfigSource {
    /// Reload the discovered configuration file for every new run.
    Filesystem,
    /// Reuse an explicit immutable snapshot.
    Snapshot(Arc<ToolsConfig>),
}

impl ToolsConfigSource {
    #[must_use]
    pub fn snapshot(config: ToolsConfig) -> Self {
        Self::Snapshot(Arc::new(config))
    }

    pub fn load(&self) -> Result<ToolsConfig> {
        match self {
            Self::Filesystem => crate::discover_and_load().map(|config| config.tools),
            Self::Snapshot(config) => Ok(config.as_ref().clone()),
        }
    }
}
